use crate::audio::{AudioBuffer, AudioSpec};
use crate::cli::{PlayArgs, RecordArgs};
use crate::effects::{Effect, EffectChain};
use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, SupportedStreamConfig,
    SupportedStreamConfigRange,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub fn list_devices() -> Result<()> {
    let host = cpal::default_host();
    let default_output = host.default_output_device();
    let default_input = host.default_input_device();
    let default_output_name = default_output.as_ref().map(ToString::to_string);
    let default_input_name = default_input.as_ref().map(ToString::to_string);

    println!(
        "Output devices (default: {}):",
        default_output
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "none".to_string())
    );
    for device in host
        .output_devices()
        .context("failed to enumerate output devices")?
    {
        let device_name = device.to_string();
        let default = if Some(device_name.as_str()) == default_output_name.as_deref() {
            " [default]"
        } else {
            ""
        };
        let details = device
            .default_output_config()
            .map(|config| {
                format!(
                    ", {} channels, {} Hz, {}",
                    config.channels(),
                    config.sample_rate(),
                    config.sample_format()
                )
            })
            .unwrap_or_default();
        println!("  {device_name}{default}{details}");
    }

    println!(
        "Input devices (default: {}):",
        default_input
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "none".to_string())
    );
    for device in host
        .input_devices()
        .context("failed to enumerate input devices")?
    {
        let device_name = device.to_string();
        let default = if Some(device_name.as_str()) == default_input_name.as_deref() {
            " [default]"
        } else {
            ""
        };
        let details = device
            .default_input_config()
            .map(|config| {
                format!(
                    ", {} channels, {} Hz, {}",
                    config.channels(),
                    config.sample_rate(),
                    config.sample_format()
                )
            })
            .unwrap_or_default();
        println!("  {device_name}{default}{details}");
    }

    Ok(())
}

pub fn play_file(args: PlayArgs) -> Result<()> {
    let stop_file = std::env::var_os("SOUNDX_STOP_FILE").map(std::path::PathBuf::from);
    if args.inputs.is_empty() {
        bail!("play requires at least one input file");
    }
    let host = cpal::default_host();
    let device = select_device(
        host.output_devices()?,
        host.default_output_device(),
        args.device.as_deref(),
        "output",
    )?;
    let default_config = device
        .default_output_config()
        .context("selected output device has no default configuration")?;
    let supported = if args.rate.is_some() || args.channels.is_some() {
        choose_stream_config(
            device.supported_output_configs()?,
            default_config,
            args.rate,
            args.channels,
            "output",
        )?
    } else {
        default_config
    };
    let format = supported.sample_format();
    let config = supported.config();
    let output_rate = config.sample_rate;
    let output_channels = config.channels;
    if output_rate == 0 || output_channels == 0 {
        bail!("selected output device reported an invalid stream configuration");
    }

    let mut audio = AudioBuffer {
        spec: AudioSpec {
            sample_rate: output_rate,
            channels: output_channels,
        },
        samples: Vec::new(),
    };
    for input in &args.inputs {
        let mut next = AudioBuffer::read(input)
            .with_context(|| format!("failed to read {}", input.display()))?;
        EffectChain::new(vec![
            Effect::Rate {
                sample_rate: output_rate,
            },
            Effect::Channels {
                channels: output_channels,
            },
        ])
        .apply(&mut next)
        .with_context(|| format!("failed to adapt {} for the output device", input.display()))?;
        append_playback_audio(&mut audio, next)?;
    }

    if audio.samples.is_empty() {
        return Ok(());
    }

    eprintln!(
        "Playing {} file(s) on {} ({} Hz, {} channels){}",
        args.inputs.len(),
        device,
        output_rate,
        output_channels,
        if args.loop_play {
            "; repeat until Ctrl+C"
        } else {
            ""
        }
    );
    let samples = Arc::new(audio.samples);
    let cursor = Arc::new(AtomicUsize::new(0));
    let stream_error = Arc::new(Mutex::new(None));
    let stream = make_output_stream(
        &device,
        config,
        format,
        samples.clone(),
        cursor.clone(),
        stream_error.clone(),
        args.loop_play,
    )?;
    let running = Arc::new(AtomicBool::new(true));
    if args.loop_play {
        let stop = running.clone();
        ctrlc::set_handler(move || stop.store(false, Ordering::Release))
            .context("failed to install Ctrl+C handler for repeated playback")?;
    }
    stream.play().context("failed to start output stream")?;

    let playback_seconds =
        samples.len() as f64 / (f64::from(output_rate) * f64::from(output_channels));
    let wait_limit = playback_wait_limit(playback_seconds, args.loop_play);
    let started = Instant::now();
    while if args.loop_play {
        running.load(Ordering::Acquire)
    } else {
        cursor.load(Ordering::Relaxed) < samples.len()
    } {
        if let Some(error) = take_stream_error(&stream_error) {
            bail!("output stream failed: {error}");
        }
        if stop_file.as_ref().is_some_and(|path| path.is_file()) {
            break;
        }
        if wait_limit.is_some_and(|limit| started.elapsed() > limit) {
            bail!("output stream did not consume the requested audio in time");
        }
        thread::sleep(Duration::from_millis(5));
    }
    // Let the finite final callback reach the host mixer before dropping the stream.
    if !args.loop_play {
        thread::sleep(Duration::from_millis(250));
    }
    if let Some(error) = take_stream_error(&stream_error) {
        bail!("output stream failed: {error}");
    }
    Ok(())
}

pub fn record_file(args: RecordArgs) -> Result<()> {
    let stop_file = std::env::var_os("SOUNDX_STOP_FILE").map(std::path::PathBuf::from);
    if args.continuous {
        validate_continuous_wav_output(&args.output)?;
    }
    if !args.continuous
        && (!args.duration.is_finite() || args.duration <= 0.0 || args.duration > 86_400.0)
    {
        bail!(
            "recording duration must be finite, greater than zero, and no more than 86400 seconds"
        );
    }

    let host = cpal::default_host();
    let device = select_device(
        host.input_devices()?,
        host.default_input_device(),
        args.device.as_deref(),
        "input",
    )?;
    let default_config = device
        .default_input_config()
        .context("selected input device has no default configuration")?;
    let supported = if args.rate.is_some() || args.channels.is_some() {
        choose_stream_config(
            device.supported_input_configs()?,
            default_config,
            args.rate,
            args.channels,
            "input",
        )?
    } else {
        default_config
    };
    let format = supported.sample_format();
    let config = supported.config();
    let sample_rate = config.sample_rate;
    let channels = config.channels;
    if sample_rate == 0 || channels == 0 {
        bail!("selected input device reported an invalid stream configuration");
    }
    if args.continuous {
        return record_continuous(args, device, config, format, sample_rate, channels);
    }
    let frame_count_f = (args.duration * f64::from(sample_rate)).round().max(1.0);
    if frame_count_f > usize::MAX as f64 {
        bail!("recording duration is too large");
    }
    let frames = frame_count_f as usize;
    let target_samples = frames
        .checked_mul(usize::from(channels))
        .ok_or_else(|| anyhow!("recording is too large"))?;
    let capture = Arc::new(Mutex::new(Vec::new()));
    capture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .try_reserve_exact(target_samples)
        .context("not enough memory to reserve the recording buffer")?;
    let finished = Arc::new(AtomicBool::new(false));
    let stream_error = Arc::new(Mutex::new(None));
    let stream = make_input_stream(
        &device,
        config,
        format,
        target_samples,
        capture.clone(),
        finished.clone(),
        stream_error.clone(),
    )?;

    eprintln!(
        "Recording from {} ({} Hz, {} channels) for {:.3} seconds to {}",
        device,
        sample_rate,
        channels,
        args.duration,
        args.output.display()
    );
    let started = Instant::now();
    stream.play().context("failed to start input stream")?;
    let wait_limit = Duration::from_secs_f64(args.duration + 10.0);
    while !finished.load(Ordering::Acquire) {
        if stop_file.as_ref().is_some_and(|path| path.is_file()) {
            bail!("finite recording stopped before its requested duration; no output written");
        }
        if let Some(error) = take_stream_error(&stream_error) {
            bail!("input stream failed: {error}");
        }
        if started.elapsed() > wait_limit {
            bail!("input stream did not deliver the requested audio in time");
        }
        thread::sleep(Duration::from_millis(5));
    }
    drop(stream);

    if let Some(error) = take_stream_error(&stream_error) {
        bail!("input stream failed: {error}");
    }
    let samples = {
        let mut capture = capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *capture)
    };
    if samples.len() != target_samples {
        bail!(
            "input stream returned {} samples; expected {target_samples}",
            samples.len()
        );
    }

    let audio = AudioBuffer {
        spec: AudioSpec {
            sample_rate,
            channels,
        },
        samples,
    };
    crate::encode::write_audio(&audio, &args.output, None, false, 192)
        .with_context(|| format!("failed to write {}", args.output.display()))
}

fn record_continuous(
    args: RecordArgs,
    device: cpal::Device,
    config: cpal::StreamConfig,
    format: SampleFormat,
    sample_rate: u32,
    channels: u16,
) -> Result<()> {
    let stop_file = std::env::var_os("SOUNDX_STOP_FILE").map(std::path::PathBuf::from);
    if let Some(parent) = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }
    let (sender, receiver) = sync_channel::<Vec<f32>>(8);
    let output = args.output.clone();
    let writer =
        thread::spawn(move || write_continuous_wav_worker(output, receiver, sample_rate, channels));
    let running = Arc::new(AtomicBool::new(true));
    let overflow = Arc::new(AtomicBool::new(false));
    let stream_error = Arc::new(Mutex::new(None));
    let stream = match make_continuous_input_stream(
        &device,
        config,
        format,
        sender.clone(),
        running.clone(),
        overflow.clone(),
        stream_error.clone(),
    ) {
        Ok(stream) => stream,
        Err(error) => {
            drop(sender);
            let _ = writer.join();
            return Err(error);
        }
    };
    let stop = running.clone();
    if let Err(error) = ctrlc::set_handler(move || stop.store(false, Ordering::Release)) {
        drop(stream);
        drop(sender);
        let _ = writer.join();
        return Err(error).context("failed to install Ctrl+C handler for continuous recording");
    }
    eprintln!(
        "Recording continuously from {} ({} Hz, {} channels) to {}; press Ctrl+C to stop",
        device,
        sample_rate,
        channels,
        args.output.display()
    );
    if let Err(error) = stream.play() {
        drop(stream);
        drop(sender);
        let _ = writer.join();
        return Err(error).context("failed to start input stream");
    }

    let mut capture_error = None;
    while running.load(Ordering::Acquire) {
        if stop_file.as_ref().is_some_and(|path| path.is_file()) {
            running.store(false, Ordering::Release);
            break;
        }
        if let Some(error) = take_stream_error(&stream_error) {
            capture_error = Some(format!("input stream failed: {error}"));
            running.store(false, Ordering::Release);
            break;
        }
        if overflow.load(Ordering::Acquire) {
            capture_error = Some(
                "recording writer fell behind; stopped to avoid silently dropping audio"
                    .to_string(),
            );
            running.store(false, Ordering::Release);
            break;
        }
        if writer.is_finished() {
            capture_error = Some("continuous WAV writer stopped unexpectedly".to_string());
            running.store(false, Ordering::Release);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if overflow.load(Ordering::Acquire) {
        capture_error.get_or_insert_with(|| {
            "recording writer fell behind; stopped to avoid silently dropping audio".to_string()
        });
    }
    drop(stream);
    drop(sender);
    let writer_result = writer
        .join()
        .map_err(|_| anyhow!("continuous WAV writer thread panicked"))
        .and_then(|result| result);
    if let Some(error) = take_stream_error(&stream_error) {
        capture_error.get_or_insert_with(|| format!("input stream failed: {error}"));
    }
    match (writer_result, capture_error) {
        (Ok(written), Some(error)) => bail!(
            "{error}; finalized a partial recording with {written} samples at {}",
            args.output.display()
        ),
        (Err(writer_error), Some(capture_error)) => bail!(
            "{capture_error}; continuous WAV writer failed: {writer_error}; output is at {}",
            args.output.display()
        ),
        (Err(writer_error), None) => {
            return Err(writer_error)
                .with_context(|| format!("failed to write {}", args.output.display()));
        }
        (Ok(written), None) => {
            eprintln!("Saved {written} samples to {}", args.output.display());
        }
    }
    Ok(())
}

fn validate_continuous_wav_output(output: &std::path::Path) -> Result<()> {
    if output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("wav"))
    {
        bail!("continuous recording currently streams 16-bit PCM WAV output only");
    }
    Ok(())
}

fn make_continuous_input_stream(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: SampleFormat,
    sender: SyncSender<Vec<f32>>,
    running: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    stream_error: Arc<Mutex<Option<String>>>,
) -> Result<Stream> {
    macro_rules! build {
        ($sample:ty) => {
            continuous_input_stream::<$sample>(
                device,
                config,
                sender,
                running,
                overflow,
                stream_error,
            )
        };
    }
    match format {
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(cpal::I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(cpal::U24),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        unsupported => bail!("unsupported input sample format {unsupported}"),
    }
}

fn playback_wait_limit(playback_seconds: f64, loop_play: bool) -> Option<Duration> {
    (!loop_play).then(|| Duration::from_secs_f64(playback_seconds + 10.0))
}

fn append_playback_audio(target: &mut AudioBuffer, mut next: AudioBuffer) -> Result<()> {
    if target.spec.sample_rate != next.spec.sample_rate
        || target.spec.channels != next.spec.channels
    {
        bail!("playback inputs must be converted to the same device format before concatenation");
    }
    if usize::from(next.spec.channels) == 0
        || !next
            .samples
            .len()
            .is_multiple_of(usize::from(next.spec.channels))
    {
        bail!("playback input sample count is not aligned to its channel count");
    }
    if target.samples.is_empty() {
        target.samples = next.samples;
    } else {
        target
            .samples
            .try_reserve(next.samples.len())
            .context("not enough memory to concatenate playback inputs")?;
        target.samples.append(&mut next.samples);
    }
    Ok(())
}

fn write_continuous_wav_worker(
    output: std::path::PathBuf,
    receiver: std::sync::mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    channels: u16,
) -> Result<u64> {
    let sample_limit = continuous_wav_sample_limit(channels)?;
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let writer = hound::WavWriter::create(&output, spec)
        .with_context(|| format!("failed to create {}", output.display()))?;
    write_continuous_wav(writer, receiver, sample_limit)
}

fn continuous_wav_sample_limit(channels: u16) -> Result<u64> {
    if channels == 0 {
        bail!("continuous WAV recording requires at least one channel");
    }
    // Hound writes a 44-byte PCM header or a 68-byte extensible header.
    // RIFF's size excludes its first 8 bytes. Reserve the header and stop on
    // a complete frame before either 32-bit size field can overflow.
    let riff_header_size = if channels > 2 { 60 } else { 36 };
    let channels = u64::from(channels);
    Ok((u64::from(u32::MAX) - riff_header_size) / (2 * channels) * channels)
}

fn write_continuous_wav<W: std::io::Write + std::io::Seek>(
    mut writer: hound::WavWriter<W>,
    receiver: std::sync::mpsc::Receiver<Vec<f32>>,
    sample_limit: u64,
) -> Result<u64> {
    let channels = u64::from(writer.spec().channels);
    let sample_limit = sample_limit.min(continuous_wav_sample_limit(writer.spec().channels)?);
    let sample_limit = sample_limit / channels * channels;
    let mut written = 0_u64;
    let write_result = (|| -> Result<()> {
        while let Ok(samples) = receiver.recv() {
            if !(samples.len() as u64).is_multiple_of(channels) {
                bail!("continuous input sample count is not aligned to its channel count");
            }
            let count = (samples.len() as u64).min(sample_limit - written) as u32;
            if count > 0 {
                // Reuse Hound's PCM16 buffer and check I/O once per chunk.
                let mut chunk = writer.get_i16_writer(count);
                for &sample in &samples[..count as usize] {
                    let pcm = crate::audio::quantize_pcm(sample.clamp(-1.0, 1.0), 16) as i16;
                    chunk.write_sample(pcm);
                }
                chunk.flush()?;
                written += u64::from(count);
            }
            if u64::from(count) < samples.len() as u64 {
                bail!("continuous recording reached the RIFF WAV size limit");
            }
        }
        Ok(())
    })();
    match (write_result, writer.finalize()) {
        (Ok(()), Ok(())) => Ok(written),
        (Err(error), Ok(())) => {
            Err(error.context(format!("finalized partial WAV with {written} samples")))
        }
        (Ok(()), Err(error)) => Err(error).context("failed to finalize continuous WAV"),
        (Err(error), Err(finalize_error)) => {
            bail!("{error}; failed to finalize continuous WAV: {finalize_error}")
        }
    }
}

fn continuous_input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    sender: SyncSender<Vec<f32>>,
    running: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    stream_error: Arc<Mutex<Option<String>>>,
) -> Result<Stream>
where
    T: SizedSample + Copy,
    f32: FromSample<T>,
{
    let error_state = stream_error.clone();
    let stop_on_error = running.clone();
    device
        .build_input_stream::<T, _, _>(
            config,
            move |input, _| {
                if !running.load(Ordering::Relaxed) {
                    return;
                }
                let chunk = input
                    .iter()
                    .map(|sample| f32::from_sample(*sample).clamp(-1.0, 1.0))
                    .collect();
                match sender.try_send(chunk) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        overflow.store(true, Ordering::Release);
                        running.store(false, Ordering::Release);
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        running.store(false, Ordering::Release);
                    }
                }
            },
            move |error| {
                set_stream_error(&error_state, error.to_string());
                stop_on_error.store(false, Ordering::Release);
            },
            None,
        )
        .context("failed to create continuous input audio stream")
}

fn select_device(
    devices: impl Iterator<Item = cpal::Device>,
    default: Option<cpal::Device>,
    requested_name: Option<&str>,
    direction: &str,
) -> Result<cpal::Device> {
    if let Some(requested_name) = requested_name {
        return devices
            .filter_map(|device| {
                let name = device.to_string();
                name.eq_ignore_ascii_case(requested_name).then_some(device)
            })
            .next()
            .ok_or_else(|| {
                anyhow!(
                    "no {direction} device named '{requested_name}'; run `soundx devices` to list available devices"
                )
            });
    }
    default.ok_or_else(|| anyhow!("no default {direction} audio device is available"))
}

fn choose_stream_config(
    configs: impl Iterator<Item = SupportedStreamConfigRange>,
    default: SupportedStreamConfig,
    requested_rate: Option<u32>,
    requested_channels: Option<u16>,
    direction: &str,
) -> Result<SupportedStreamConfig> {
    if requested_rate == Some(0) || requested_channels == Some(0) {
        bail!("requested {direction} sample rate and channel count must be greater than zero");
    }
    let target_rate = requested_rate.unwrap_or(default.sample_rate());
    let target_channels = requested_channels.unwrap_or(default.channels());
    let default_format = default.sample_format();
    let mut compatible: Vec<_> = configs
        .filter(|config| config.channels() == target_channels)
        .filter_map(|config| {
            let min_rate = config.min_sample_rate();
            let max_rate = config.max_sample_rate();
            let selected_rate = if requested_rate.is_some() {
                (min_rate <= target_rate && target_rate <= max_rate).then_some(target_rate)?
            } else {
                target_rate.clamp(min_rate, max_rate)
            };
            Some((config, selected_rate))
        })
        .collect();
    compatible.sort_by_key(|(config, rate)| {
        (
            config.sample_format() != default_format,
            rate.abs_diff(target_rate),
        )
    });
    let Some((selected, sample_rate)) = compatible.into_iter().next() else {
        let rate_description = requested_rate
            .map(|rate| format!("{rate} Hz"))
            .unwrap_or_else(|| format!("the default {target_rate} Hz"));
        bail!(
            "no {direction} device configuration supports {target_channels} channels at {rate_description}"
        );
    };
    Ok(selected.with_sample_rate(sample_rate))
}

fn make_output_stream(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: SampleFormat,
    samples: Arc<Vec<f32>>,
    cursor: Arc<AtomicUsize>,
    stream_error: Arc<Mutex<Option<String>>>,
    loop_play: bool,
) -> Result<Stream> {
    macro_rules! build {
        ($sample:ty) => {
            output_stream::<$sample>(device, config, samples, cursor, stream_error, loop_play)
        };
    }
    match format {
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(cpal::I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(cpal::U24),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        unsupported => bail!("unsupported output sample format {unsupported}"),
    }
}

fn output_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    samples: Arc<Vec<f32>>,
    cursor: Arc<AtomicUsize>,
    stream_error: Arc<Mutex<Option<String>>>,
    loop_play: bool,
) -> Result<Stream>
where
    T: SizedSample + Sample + FromSample<f32>,
{
    let error_state = stream_error.clone();
    let mut position = 0;
    device
        .build_output_stream::<T, _, _>(
            config,
            move |output, _| {
                fill_playback_buffer(output, &samples, &mut position, loop_play);
                cursor.store(position, Ordering::Relaxed);
            },
            move |error| set_stream_error(&error_state, error.to_string()),
            None,
        )
        .context("failed to create output audio stream")
}

fn fill_playback_buffer<T: Sample + FromSample<f32>>(
    output: &mut [T],
    samples: &[f32],
    position: &mut usize,
    loop_play: bool,
) {
    for target in output {
        if loop_play && *position == samples.len() {
            *position = 0;
        }
        let value = samples.get(*position).copied().unwrap_or(0.0);
        *target = T::from_sample(value.clamp(-1.0, 1.0));
        if *position < samples.len() {
            *position += 1;
        }
    }
}

fn make_input_stream(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: SampleFormat,
    target_samples: usize,
    capture: Arc<Mutex<Vec<f32>>>,
    finished: Arc<AtomicBool>,
    stream_error: Arc<Mutex<Option<String>>>,
) -> Result<Stream> {
    macro_rules! build {
        ($sample:ty) => {
            input_stream::<$sample>(
                device,
                config,
                target_samples,
                capture,
                finished,
                stream_error,
            )
        };
    }
    match format {
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(cpal::I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(cpal::U24),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        unsupported => bail!("unsupported input sample format {unsupported}"),
    }
}

fn input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    target_samples: usize,
    capture: Arc<Mutex<Vec<f32>>>,
    finished: Arc<AtomicBool>,
    stream_error: Arc<Mutex<Option<String>>>,
) -> Result<Stream>
where
    T: SizedSample + Copy,
    f32: FromSample<T>,
{
    let error_state = stream_error.clone();
    device
        .build_input_stream::<T, _, _>(
            config,
            move |input, _| {
                let mut samples = capture
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let remaining = target_samples.saturating_sub(samples.len());
                samples.extend(
                    input
                        .iter()
                        .take(remaining)
                        .map(|sample| f32::from_sample(*sample).clamp(-1.0, 1.0)),
                );
                if samples.len() >= target_samples {
                    finished.store(true, Ordering::Release);
                }
            },
            move |error| set_stream_error(&error_state, error.to_string()),
            None,
        )
        .context("failed to create input audio stream")
}

fn set_stream_error(state: &Mutex<Option<String>>, error: String) {
    *state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
}

fn take_stream_error(state: &Mutex<Option<String>>) -> Option<String> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "soundx-device-{}-{}.wav",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn repeated_playback_has_no_single_pass_deadline() {
        assert_eq!(playback_wait_limit(0.05, true), None);
        assert_eq!(
            playback_wait_limit(0.05, false),
            Some(Duration::from_millis(10_050))
        );
        assert_eq!(
            playback_wait_limit(90_000.0, false),
            Some(Duration::from_secs(90_010))
        );
    }

    #[test]
    fn playback_wraps_across_callbacks_without_an_unbounded_cursor() {
        let samples = [0.1, 0.2, 0.3, 0.4];
        let mut position = 0;
        let mut first = [0.0_f32; 6];
        fill_playback_buffer(&mut first, &samples, &mut position, true);
        assert_eq!(first, [0.1, 0.2, 0.3, 0.4, 0.1, 0.2]);
        let mut second = [0.0_f32; 4];
        fill_playback_buffer(&mut second, &samples, &mut position, true);
        assert_eq!(second, [0.3, 0.4, 0.1, 0.2]);
        assert_eq!(position, 2);
    }

    #[test]
    fn finite_and_empty_playback_fill_the_remaining_buffer_with_silence() {
        let mut position = 0;
        let mut output = [0.0_f32; 4];
        fill_playback_buffer(&mut output, &[-2.0, 2.0], &mut position, false);
        assert_eq!(output, [-1.0, 1.0, 0.0, 0.0]);
        assert_eq!(position, 2);
        fill_playback_buffer(&mut output, &[-2.0, 2.0], &mut position, false);
        assert_eq!(output, [0.0; 4]);
        assert_eq!(position, 2);

        let mut position = 0;
        let mut unsigned_output = [0_u16; 4];
        fill_playback_buffer(&mut unsigned_output, &[], &mut position, true);
        assert_eq!(unsigned_output, [32_768; 4]);
        assert_eq!(position, 0);
    }

    #[test]
    fn playlist_accepts_the_first_track_into_an_empty_buffer() {
        let spec = AudioSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        let mut combined = AudioBuffer {
            spec,
            samples: Vec::new(),
        };
        append_playback_audio(
            &mut combined,
            AudioBuffer {
                spec,
                samples: vec![0.1, 0.2],
            },
        )
        .unwrap();
        assert_eq!(combined.samples, [0.1, 0.2]);
        assert!(
            append_playback_audio(
                &mut combined,
                AudioBuffer {
                    spec,
                    samples: vec![0.3]
                }
            )
            .is_err()
        );
        assert_eq!(combined.samples, [0.1, 0.2]);
    }

    #[test]
    fn continuous_wav_limit_reserves_the_header_and_complete_frames() {
        for channels in [1_u16, 2, 6] {
            let limit = continuous_wav_sample_limit(channels).unwrap();
            let header = if channels > 2 { 60 } else { 36 };
            assert!(limit.is_multiple_of(u64::from(channels)));
            assert!(limit * 2 + header <= u64::from(u32::MAX));
            assert!((limit + u64::from(channels)) * 2 + header > u64::from(u32::MAX));
        }
        assert!(continuous_wav_sample_limit(0).is_err());
    }

    #[test]
    fn continuous_wav_keeps_a_readable_prefix_on_size_or_alignment_errors() {
        for (chunks, limit, expected_samples, message) in [
            (
                vec![vec![0.5; 4], vec![0.25; 4]],
                6,
                6,
                "RIFF WAV size limit",
            ),
            (vec![vec![0.5; 4], vec![0.25; 3]], 100, 4, "not aligned"),
        ] {
            let mut bytes = std::io::Cursor::new(Vec::new());
            let writer = hound::WavWriter::new(
                &mut bytes,
                hound::WavSpec {
                    channels: 2,
                    sample_rate: 8_000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            let (sender, receiver) = sync_channel(chunks.len());
            for chunk in chunks {
                sender.send(chunk).unwrap();
            }
            drop(sender);
            let error = write_continuous_wav(writer, receiver, limit).unwrap_err();
            assert!(format!("{error:#}").contains(message), "{error:#}");
            let bytes = bytes.into_inner();
            let riff_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            assert_eq!(riff_size as usize + 8, bytes.len());
            let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
            assert_eq!(reader.duration(), expected_samples / 2);
            assert_eq!(
                reader
                    .into_samples::<i16>()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
                    .len(),
                expected_samples as usize
            );
        }
    }

    #[test]
    fn multi_file_playback_concatenates_aligned_device_buffers() {
        let mut combined = AudioBuffer {
            spec: AudioSpec {
                sample_rate: 48_000,
                channels: 2,
            },
            samples: vec![0.1, 0.2],
        };
        let next = AudioBuffer {
            spec: combined.spec,
            samples: vec![0.3, 0.4, 0.5, 0.6],
        };
        append_playback_audio(&mut combined, next).unwrap();
        assert_eq!(combined.samples, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
        let incompatible = AudioBuffer {
            spec: AudioSpec {
                sample_rate: 44_100,
                channels: 2,
            },
            samples: vec![0.0, 0.0],
        };
        assert!(append_playback_audio(&mut combined, incompatible).is_err());
    }

    #[test]
    fn continuous_recording_requires_wav_output_case_insensitively() {
        assert!(validate_continuous_wav_output(std::path::Path::new("capture.WAV")).is_ok());
        assert!(validate_continuous_wav_output(std::path::Path::new("capture.mp3")).is_err());
        assert!(validate_continuous_wav_output(std::path::Path::new("capture")).is_err());
    }

    #[test]
    fn continuous_wav_writer_drains_chunks_and_finalizes_header() {
        let output = test_path();
        let (sender, receiver) = sync_channel(2);
        sender.send(vec![0.5, -0.5]).unwrap();
        sender.send(vec![0.25, -0.25]).unwrap();
        drop(sender);
        assert_eq!(
            write_continuous_wav_worker(output.clone(), receiver, 8_000, 2).unwrap(),
            4
        );
        let reader = hound::WavReader::open(&output).unwrap();
        assert_eq!(reader.spec().channels, 2);
        assert_eq!(reader.spec().sample_rate, 8_000);
        assert_eq!(reader.duration(), 2);
        let _ = std::fs::remove_file(output);
    }
}
