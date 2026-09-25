use hound::{SampleFormat, WavSpec, WavWriter};
use std::io::Write;
use std::path::Path;
use std::process::Command;

#[test]
fn converts_with_sox_style_effects() {
    let temp = std::env::temp_dir().join(format!("soundx-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output = temp.join("output.wav");
    write_test_wav(&input, 4410, 1, 0.5);

    let status = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .arg(&input)
        .arg(&output)
        .args([
            "gain", "-6", "trim", "0", "0.01", "pad", "0.01", "0.01", "speed", "1.5", "rate",
            "22050", "lowpass", "8000", "highpass", "20", "limiter", "0.8",
        ])
        .status()
        .unwrap();

    assert!(status.success());
    let reader = hound::WavReader::open(output).unwrap();
    assert_eq!(reader.spec().sample_rate, 22050);
}

#[test]
fn parses_sox_style_echo_taps_and_tremolo() {
    let temp = std::env::temp_dir().join(format!("soundx-effect-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output = temp.join("echo.wav");
    write_test_wav(&input, 4410, 2, 0.35);

    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .arg(&input)
        .arg(&output)
        .args([
            "echo", "0.8", "0.88", "60", "0.4", "120", "0.3", "tremolo", "5", "70",
        ])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let reader = hound::WavReader::open(output).unwrap();
    assert!(reader.duration() > 4410 * 2);
}

#[test]
fn parses_sox_biquad_effects_and_width_suffixes() {
    let temp = std::env::temp_dir().join(format!("soundx-biquad-cli-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output = temp.join("filtered.wav");
    write_test_wav(&input, 4410, 2, 0.25);

    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .arg(&input)
        .arg(&output)
        .args([
            "bass",
            "3",
            "100",
            "0.5s",
            "bass",
            "0",
            "treble",
            "-2",
            "3000",
            "0.7q",
            "bandpass",
            "-c",
            "1000",
            "200h",
            "bandreject",
            "2000",
            "1q",
            "allpass",
            "500",
            "0.5o",
            "equalizer",
            "1000",
            "1k",
            "2",
            "lowpass",
            "-1",
            "8000",
            "highpass",
            "-2",
            "30",
            "0.707q",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut reader = hound::WavReader::open(output).unwrap();
    assert_eq!(reader.duration(), 4410);
    assert!(reader.samples::<i16>().all(|sample| sample.is_ok()));
}

#[test]
fn parses_sox_style_delay_downsample_upsample_repeat_and_swap() {
    let temp = std::env::temp_dir().join(format!("soundx-compat-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output = temp.join("result.wav");
    write_test_wav(&input, 4410, 2, 0.25);
    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .arg(&input)
        .arg(&output)
        .args([
            "delay",
            "0.001",
            "0.002",
            "swap",
            "downsample",
            "2",
            "upsample",
            "2",
            "repeat",
            "1",
        ])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let reader = hound::WavReader::open(output).unwrap();
    assert_eq!(reader.spec().sample_rate, 44_100);
    assert_eq!(reader.spec().channels, 2);
    assert_eq!(reader.duration(), 2 * (4410 + 88));
}

#[test]
fn lists_format_support() {
    let output = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["formats"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let formats = String::from_utf8(output.stdout).unwrap();
    assert!(formats.contains("flac"));
    assert!(formats.contains("mp3"));
    assert!(formats.contains("ogg/vorbis"));
    assert!(formats.contains("aiff"));
    assert!(formats.contains("wav (PCM, float, IMA/MS ADPCM)"));
    assert!(formats.contains("GSM 06.10"));
    assert!(formats.contains("AMR-NB/WB"));
    assert!(formats.contains("WavPack v5"));
    assert!(formats.contains("stretch, tempo, dither, compand, reverb"));
}

#[test]
fn device_commands_expose_rate_and_channel_selection() {
    for command in ["play", "record"] {
        let output = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--device"), "{command}: {help}");
        assert!(help.contains("--rate"), "{command}: {help}");
        assert!(help.contains("--channels"), "{command}: {help}");
        if command == "play" {
            assert!(help.contains("--loop"), "{command}: {help}");
        } else {
            assert!(help.contains("--continuous"), "{command}: {help}");
        }
    }
}

#[test]
fn encodes_and_reads_native_rust_output_formats() {
    let temp = std::env::temp_dir().join(format!("soundx-codec-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    write_test_wav(&input, 8820, 2, 0.35);

    for extension in ["flac", "mp3", "ogg", "aac", "aiff", "au"] {
        let output_path = temp.join(format!("encoded.{extension}"));
        let convert = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args(["convert", "--bitrate-kbps", "128"])
            .arg(&input)
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            convert.status.success(),
            "{extension}: {}",
            String::from_utf8_lossy(&convert.stderr)
        );
        assert!(std::fs::metadata(&output_path).unwrap().len() > 32);

        let info = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args(["info", "--json"])
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            info.status.success(),
            "{extension}: {}",
            String::from_utf8_lossy(&info.stderr)
        );
        let report = String::from_utf8(info.stdout).unwrap();
        assert!(report.contains("\"channels\": 2"), "{extension}: {report}");
        assert!(
            report.contains("\"sample_rate\": 44100"),
            "{extension}: {report}"
        );
    }
}

#[test]
fn encodes_and_reads_aiff_pcm_depths() {
    let temp = std::env::temp_dir().join(format!("soundx-aiff-depth-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    write_test_wav(&input, 100, 2, 0.35);

    for bits in [8_u16, 16, 24, 32] {
        let output = temp.join(format!("output-{bits}.aiff"));
        let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args(["convert", "--aiff-bits", &bits.to_string()])
            .arg(&input)
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{bits} bit: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(u16::from_be_bytes([bytes[26], bytes[27]]), bits);
        assert_eq!(bytes.len(), 54 + 200 * usize::from(bits / 8));

        let info = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args(["info", "--json"])
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            info.status.success(),
            "{bits} bit: {}",
            String::from_utf8_lossy(&info.stderr)
        );
        let report = String::from_utf8(info.stdout).unwrap();
        assert!(report.contains("\"channels\": 2"), "{bits} bit: {report}");
        assert!(
            report.contains("\"sample_rate\": 44100"),
            "{bits} bit: {report}"
        );
    }
}

#[test]
fn encodes_and_reads_all_supported_au_encodings() {
    let temp = std::env::temp_dir().join(format!("soundx-au-encoding-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    write_test_wav(&input, 8820, 2, 0.35);

    for encoding in [
        "pcm8", "pcm16", "pcm24", "pcm32", "float32", "float64", "mu-law", "a-law",
    ] {
        let output_path = temp.join(format!("encoded-{encoding}.au"));
        let convert = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args(["convert", "--au-encoding", encoding])
            .arg(&input)
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            convert.status.success(),
            "{encoding}: {}",
            String::from_utf8_lossy(&convert.stderr)
        );
        assert!(std::fs::metadata(&output_path).unwrap().len() > 28);

        let info = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args(["info", "--json"])
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            info.status.success(),
            "{encoding}: {}",
            String::from_utf8_lossy(&info.stderr)
        );
        let report = String::from_utf8(info.stdout).unwrap();
        assert!(report.contains("\"channels\": 2"), "{encoding}: {report}");
        assert!(
            report.contains("\"sample_rate\": 44100"),
            "{encoding}: {report}"
        );
    }
}

#[test]
fn encodes_and_reads_supported_raw_sample_encodings() {
    let temp =
        std::env::temp_dir().join(format!("soundx-raw-encoding-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    write_test_wav(&input, 100, 2, 0.35);

    for encoding in [
        "pcm-u8",
        "pcm-u16le",
        "pcm-u16be",
        "pcm-u24le",
        "pcm-u24be",
        "pcm-u32le",
        "pcm-u32be",
        "pcm-s8",
        "pcm-s16le",
        "pcm-s16be",
        "pcm-s24le",
        "pcm-s24be",
        "pcm-s32le",
        "pcm-s32be",
        "float32le",
        "float32be",
        "float64le",
        "float64be",
        "mu-law",
        "a-law",
    ] {
        let raw = temp.join(format!("encoded-{encoding}.raw"));
        let wav = temp.join(format!("decoded-{encoding}.wav"));
        let encode = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args(["convert", "--output-raw-encoding", encoding])
            .arg(&input)
            .arg(&raw)
            .output()
            .unwrap();
        assert!(
            encode.status.success(),
            "{encoding}: {}",
            String::from_utf8_lossy(&encode.stderr)
        );
        assert!(std::fs::metadata(&raw).unwrap().len() > 0);

        let decode = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args([
                "convert",
                "--input-raw-rate",
                "44100",
                "--input-raw-channels",
                "2",
                "--input-raw-encoding",
                encoding,
            ])
            .arg(&raw)
            .arg(&wav)
            .output()
            .unwrap();
        assert!(
            decode.status.success(),
            "{encoding}: {}",
            String::from_utf8_lossy(&decode.stderr)
        );
        let reader = hound::WavReader::open(wav).unwrap();
        assert_eq!(reader.spec().sample_rate, 44_100, "{encoding}");
        assert_eq!(reader.spec().channels, 2, "{encoding}");
        assert_eq!(reader.duration(), 100, "{encoding}");
    }
}

#[test]
fn raw_input_requires_headerless_stream_metadata() {
    let temp = std::env::temp_dir().join(format!("soundx-raw-options-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let raw = temp.join("input.raw");
    let wav = temp.join("output.wav");
    std::fs::write(&raw, [0_u8; 4]).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["convert", "--input-raw-rate", "44100"])
        .arg(&raw)
        .arg(&wav)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--input-raw-channels"));
    assert!(!wav.exists());
}

#[test]
fn g711_raw_encodings_match_sox_no_dither_reference_vectors() {
    let temp = std::env::temp_dir().join(format!("soundx-g711-golden-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let spec = WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(&input, spec).unwrap();
    for sample in [
        -32768_i16, -10000, -1000, -238, -1, 0, 1, 238, 1000, 10000, 32767,
    ] {
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();

    for (encoding, expected) in [
        (
            "mu-law",
            [
                0x00, 0x1c, 0x4e, 0x68, 0xff, 0xff, 0xff, 0xe8, 0xce, 0x9c, 0x80,
            ],
        ),
        (
            "a-law",
            [
                0x2a, 0x36, 0x7a, 0x5b, 0xd5, 0xd5, 0xd5, 0xda, 0xfa, 0xb6, 0xaa,
            ],
        ),
    ] {
        let output = temp.join(format!("{encoding}.raw"));
        let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args(["convert", "--output-raw-encoding", encoding])
            .arg(&input)
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{encoding}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(std::fs::read(output).unwrap(), expected, "{encoding}");
    }
}

#[test]
fn rejects_wav_bit_depth_options_for_compressed_output() {
    let temp = std::env::temp_dir().join(format!("soundx-codec-args-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output_path = temp.join("output.flac");
    write_test_wav(&input, 100, 1, 0.35);

    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["convert", "--bits", "24"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .unwrap();

    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("only valid for WAV"));
    assert!(!output_path.exists());
}

#[test]
fn prints_json_info() {
    let temp = std::env::temp_dir().join(format!("soundx-info-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    write_test_wav(&input, 4410, 1, 0.5);

    let output = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["info", "--json"])
        .arg(&input)
        .output()
        .unwrap();

    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("\"sample_rate\""));
}

#[test]
fn concatenates_and_mixes_inputs() {
    let temp = std::env::temp_dir().join(format!("soundx-combine-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let a = temp.join("a.wav");
    let b = temp.join("b.wav");
    let concat = temp.join("concat.wav");
    let mix = temp.join("mix.wav");
    write_test_wav(&a, 1000, 1, 0.4);
    write_test_wav(&b, 500, 1, 0.2);

    let concat_status = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["concat", "-o"])
        .arg(&concat)
        .arg(&a)
        .arg(&b)
        .status()
        .unwrap();
    assert!(concat_status.success());
    assert_eq!(hound::WavReader::open(&concat).unwrap().duration(), 1500);

    let mix_status = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["mix", "-o"])
        .arg(&mix)
        .arg(&a)
        .arg(&b)
        .status()
        .unwrap();
    assert!(mix_status.success());
    assert_eq!(hound::WavReader::open(&mix).unwrap().duration(), 1000);
}

#[test]
fn streams_wav_without_full_buffer_pipeline() {
    let temp = std::env::temp_dir().join(format!("soundx-stream-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output = temp.join("stream.wav");
    write_test_wav(&input, 44_100, 2, 0.9);

    let status = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args([
            "stream",
            "--gain-db=-3",
            "--fade-in",
            "0.01",
            "--fade-out",
            "0.01",
            "--limiter",
            "0.7",
        ])
        .arg(&input)
        .arg(&output)
        .status()
        .unwrap();

    assert!(status.success());
    let reader = hound::WavReader::open(output).unwrap();
    assert_eq!(reader.spec().channels, 2);
    assert_eq!(reader.duration(), 44_100);
}

#[test]
fn converts_wav_roundtrip_without_external_deps() {
    // Hermetic test: generate WAV → convert with effects → verify output
    let temp = std::env::temp_dir().join(format!("soundx-convert-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output = temp.join("output.wav");
    write_test_wav(&input, 4410, 1, 0.5);

    let status = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .arg("convert")
        .arg(&input)
        .arg(&output)
        .arg("--normalize")
        .status()
        .unwrap();

    assert!(status.success());
    let reader = hound::WavReader::open(output).unwrap();
    assert_eq!(reader.spec().sample_rate, 44_100);
    assert_eq!(reader.spec().channels, 1);
}

#[test]
fn converts_to_selectable_wav_depths() {
    let temp = std::env::temp_dir().join(format!("soundx-depth-test-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    write_test_wav(&input, 100, 1, 0.5);

    for bits in [8, 16, 24, 32] {
        let output = temp.join(format!("pcm-{bits}.wav"));
        let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .arg("convert")
            .arg(&input)
            .arg(&output)
            .args(["--bits", &bits.to_string()])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let reader = hound::WavReader::open(output).unwrap();
        assert_eq!(reader.spec().bits_per_sample, bits);
        assert_eq!(reader.spec().sample_format, SampleFormat::Int);
    }

    let output = temp.join("float.wav");
    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .arg("convert")
        .arg(&input)
        .arg(&output)
        .args(["--bits", "32", "--float"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let reader = hound::WavReader::open(output).unwrap();
    assert_eq!(reader.spec().sample_format, SampleFormat::Float);
}

#[test]
fn writes_and_reads_float64_wav() {
    let temp = std::env::temp_dir().join(format!("soundx-float64-wav-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output = temp.join("float64.wav");
    let roundtrip = temp.join("roundtrip.wav");
    write_test_wav(&input, 100, 1, 0.5);
    let source_sample = hound::WavReader::open(&input)
        .unwrap()
        .samples::<i16>()
        .next()
        .unwrap()
        .unwrap() as f32
        / 32768.0;

    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["convert", "--bits", "64", "--float"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = std::fs::read(&output).unwrap();
    assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 3);
    assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 64);
    assert_eq!(bytes.len(), 44 + 100 * 8);
    assert_eq!(
        f64::from_le_bytes(bytes[44..52].try_into().unwrap()),
        f64::from(source_sample)
    );

    let info = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["info", "--json"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        info.status.success(),
        "{}",
        String::from_utf8_lossy(&info.stderr)
    );
    let report = String::from_utf8(info.stdout).unwrap();
    assert!(report.contains("\"sample_rate\": 44100"), "{report}");
    assert!(report.contains("\"channels\": 1"), "{report}");

    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["convert", "--bits", "16"])
        .arg(&output)
        .arg(&roundtrip)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(hound::WavReader::open(roundtrip).unwrap().duration(), 100);

    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["convert", "--bits", "64"])
        .arg(&input)
        .arg(temp.join("invalid.wav"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("64-bit WAV output requires --float"));
}

#[test]
fn runs_json_plan() {
    let temp = std::env::temp_dir().join(format!("soundx-plan-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output = temp.join("planned.wav");
    let plan = temp.join("plan.json");
    write_test_wav(&input, 4410, 1, 0.5);
    let plan_text = format!(
        r#"{{
  "mode": "convert",
  "inputs": ["{}"],
  "output": "{}",
  "effects": ["trim", "0", "0.02", "silence", "-60", "0.001", "channels", "2"],
  "stat_json": true
}}"#,
        input.display().to_string().replace('\\', "\\\\"),
        output.display().to_string().replace('\\', "\\\\")
    );
    std::fs::File::create(&plan)
        .unwrap()
        .write_all(plan_text.as_bytes())
        .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args(["run-plan"])
        .arg(&plan)
        .output()
        .unwrap();

    assert!(result.status.success());
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("\"channels\": 2")
    );
    assert_eq!(hound::WavReader::open(output).unwrap().spec().channels, 2);
}

#[test]
fn synthesizes_audio_with_effects() {
    let temp = std::env::temp_dir().join(format!("soundx-synth-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let output = temp.join("tone.wav");

    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .args([
            "synth",
            "--duration",
            "0.1",
            "--freq",
            "880",
            "--channels",
            "2",
            "--waveform",
            "triangle",
            "--fade",
            "0.01",
            "--stat-json",
        ])
        .arg(&output)
        .output()
        .unwrap();

    assert!(result.status.success());
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("\"sample_rate\": 44100")
    );
    let reader = hound::WavReader::open(output).unwrap();
    assert_eq!(reader.spec().channels, 2);
    assert_eq!(reader.duration(), 4410);
}

#[test]
fn new_sox_style_effects_parse_and_run_together() {
    let temp = std::env::temp_dir().join(format!("soundx-new-effects-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    let output = temp.join("effects.wav");
    write_test_wav(&input, 8_820, 1, 0.35);
    let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
        .arg(&input)
        .arg(&output)
        .args([
            "dither",
            "16",
            "compand",
            "0,0",
            "6:-inf,-inf,-20,-20,0,-6",
            "reverb",
            "-w",
            "20",
            "50",
            "70",
            "100",
            "3",
            "-6",
            "stretch",
            "1.1",
            "50",
            "8",
            "10",
            "tempo",
            "-q",
            "1.1",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(hound::WavReader::open(output).unwrap().duration() > 8_820);
}

#[test]
fn pure_rust_codec_formats_round_trip_through_the_cli() {
    let temp = std::env::temp_dir().join(format!("soundx-codec-cli-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let input = temp.join("input.wav");
    write_test_wav(&input, 8_820, 1, 0.3);
    for (extension, adpcm) in [
        ("ima.wav", Some("ima")),
        ("ms.wav", Some("ms")),
        ("speech.gsm", None),
        ("speech.amr", None),
        ("speech-wide.awb", None),
        ("lossless.wv", None),
    ] {
        let encoded = temp.join(extension);
        let mut command = Command::new(env!("CARGO_BIN_EXE_soundx"));
        command.args(["convert"]).arg(&input).arg(&encoded);
        if let Some(adpcm) = adpcm {
            command.args(["--wav-adpcm", adpcm]);
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "encode {extension}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let decoded = temp.join(format!("decoded-{extension}.wav"));
        let result = Command::new(env!("CARGO_BIN_EXE_soundx"))
            .args(["convert"])
            .arg(&encoded)
            .arg(&decoded)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "decode {extension}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(hound::WavReader::open(decoded).unwrap().duration() > 0);
    }
}

fn write_test_wav(path: &Path, frames: usize, channels: u16, amplitude: f32) {
    let spec = WavSpec {
        channels,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec).unwrap();
    for index in 0..frames {
        let phase = index as f32 / 44_100.0 * 440.0 * std::f32::consts::TAU;
        let sample = (phase.sin() * i16::MAX as f32 * amplitude) as i16;
        for _ in 0..channels {
            writer.write_sample(sample).unwrap();
        }
    }
    writer.finalize().unwrap();
}
