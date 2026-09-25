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
        append_playback_audio(&mut audio, &next)?;
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
    let wait_limit = Duration::from_secs_f64(playback_seconds.min(86_400.0) + 10.0);
    let started = Instant::now();
    while if args.loop_play {
        running.load(Ordering::Acquire)
    } else {
        cursor.load(Ordering::Relaxed) < samples.len()
    } {
        if let Some(error) = take_stream_error(&stream_error) {
            bail!("output stream failed: {error}");
        }
        if started.elapsed() > wait_limit {
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

fn append_playback_audio(target: &mut AudioBuffer, next: &AudioBuffer) -> Result<()> {
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
    target
        .samples
        .try_reserve(next.samples.len())
        .context("not enough memory to concatenate playback inputs")?;
    target.samples.extend_from_slice(&next.samples);
    Ok(())
}

fn write_continuous_wav_worker(
    output: std::path::PathBuf,
    receiver: std::sync::mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    channels: u16,
) -> Result<u64> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&output, spec)
        .with_context(|| format!("failed to create {}", output.display()))?;
    let mut written = 0_u64;
    while let Ok(samples) = receiver.recv() {
        for sample in samples {
            let pcm = crate::audio::quantize_pcm(sample.clamp(-1.0, 1.0), 16) as i16;
            writer.write_sample(pcm)?;
            written += 1;
        }
    }
    writer.finalize()?;
    Ok(written)
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
                if matches!(
                    sender.try_send(chunk),
                    Err(TrySendError::Full(_) | TrySendError::Disconnected(_))
                ) {
                    overflow.store(true, Ordering::Release);
                    running.store(false, Ordering::Release);
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
    device
        .build_output_stream::<T, _, _>(
            config,
            move |output, _| {
                let start = cursor.fetch_add(output.len(), Ordering::Relaxed);
                for (index, target) in output.iter_mut().enumerate() {
                    let sample_index = start.saturating_add(index);
                    let sample_index = if loop_play {
                        sample_index % samples.len()
                    } else {
                        sample_index
                    };
                    let value = samples
                        .get(sample_index)
                        .copied()
                        .unwrap_or(0.0)
                        .clamp(-1.0, 1.0);
                    *target = T::from_sample(value);
                }
            },
            move |error| set_stream_error(&error_state, error.to_string()),
            None,
        )
        .context("failed to create output audio stream")
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
        append_playback_audio(&mut combined, &next).unwrap();
        assert_eq!(combined.samples, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
        let incompatible = AudioBuffer {
            spec: AudioSpec {
                sample_rate: 44_100,
                channels: 2,
            },
            samples: vec![0.0, 0.0],
        };
        assert!(append_playback_audio(&mut combined, &incompatible).is_err());
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
