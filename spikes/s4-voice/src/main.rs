//! Spike S4: real CoreAudio room capture, Earshot segmentation, and live GPT-Transcribe.
//!
//! Run from the workspace root:
//!   cargo run --release -p s4-voice -- [--room-seconds 30] [--trials 5]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use audioadapter_buffers::owned::InterleavedOwned;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use earshot::Detector;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::multipart::{Form, Part};
use rubato::{Fft, FixedSync, Resampler};
use serde::Serialize;
use serde_json::Value;

const TARGET_RATE: u32 = 16_000;
const FRAME_SAMPLES: usize = 256;
const FRAME_MS: u64 = 16;
const START_FRAMES: usize = 4;
const HANGOVER_FRAMES: usize = 34;
const MIN_SPEECH_FRAMES: usize = 16;
const PRE_ROLL_FRAMES: usize = 25;
const DEFAULT_FIXTURE: &str =
    "Stark, open Hypercanvas and make a bright blue launch advertisement for our autumn campaign.";

#[derive(Clone, Copy)]
struct Args {
    room_seconds: u64,
    trials: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    passed: bool,
    input_device: String,
    input_sample_rate: u32,
    input_channels: u16,
    room_seconds: u64,
    room_overflows: u64,
    room_sample_count: usize,
    room_peak_dbfs: f32,
    room_rms_dbfs: f32,
    room_noise_floor_dbfs: f32,
    room_segments: Vec<Segment>,
    false_triggers_per_minute: f64,
    room_vad_cpu_ms: u128,
    fixture_text: String,
    fixture_duration_ms: u64,
    fixture_noise_floor_dbfs: f32,
    fixture_segments: Vec<Segment>,
    transcription_trials: Vec<TranscriptionTrial>,
    request_p50_ms: u128,
    request_p95_ms: u128,
    projected_end_to_transcript_p50_ms: u128,
    projected_end_to_transcript_p95_ms: u128,
    projected_550ms_p50_ms: u128,
    projected_550ms_p95_ms: u128,
    speculative_350ms_p50_ms: u128,
    speculative_350ms_p95_ms: u128,
    checks: Vec<Check>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Segment {
    start_ms: u64,
    speech_end_ms: u64,
    emitted_ms: u64,
    duration_ms: u64,
    speech_frames: usize,
    peak_score: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptionTrial {
    request_ms: u128,
    encode_ms: u128,
    projected_end_to_transcript_ms: u128,
    text: String,
    languages: Vec<String>,
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

struct Capture {
    device: String,
    sample_rate: u32,
    channels: u16,
    samples: Vec<f32>,
    overflows: u64,
}

fn main() -> anyhow::Result<()> {
    load_dotenv();
    let args = parse_args()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(args))
}

async fn run(args: Args) -> anyhow::Result<()> {
    let key = std::env::var("OPENAI_API_KEY")
        .context("OPENAI_API_KEY is required for the S4 live transcription probe")?;
    let model = std::env::var("STT_MODEL").unwrap_or_else(|_| "gpt-transcribe".into());
    let report_path = std::env::var_os("NEO_VOICE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("spikes/out/s4-voice/report.json"));

    println!(
        "Capturing {} seconds from the default input for the room false-trigger sample…",
        args.room_seconds
    );
    let capture = capture_room(args.room_seconds)?;
    let room_16k = resample_mono(&capture.samples, capture.sample_rate)?;
    let room_peak_dbfs = dbfs(room_16k.iter().copied().map(f32::abs).fold(0.0, f32::max));
    let room_rms_dbfs = dbfs(rms(&room_16k));
    let room_vad_started = Instant::now();
    let (room_noise_floor_dbfs, room_segments) = segment(&room_16k);
    let room_vad_cpu_ms = room_vad_started.elapsed().as_millis();
    let false_triggers_per_minute = room_segments.len() as f64 * 60.0 / args.room_seconds as f64;

    let scratch = tempfile::tempdir()?;
    let fixture_wav = synthesize_fixture(scratch.path(), DEFAULT_FIXTURE)?;
    let fixture_audio = read_mono_wav(&fixture_wav)?;
    let fixture_duration_ms = fixture_audio.len() as u64 * 1_000 / u64::from(TARGET_RATE);
    // Production calibrates before an utterance. Add deterministic silence around the
    // synthetic clip so its first second is a valid noise-floor sample and hangover can close.
    let mut fixture_samples = Vec::with_capacity(fixture_audio.len() + TARGET_RATE as usize * 2);
    fixture_samples.resize(TARGET_RATE as usize, 0.0);
    fixture_samples.extend_from_slice(&fixture_audio);
    fixture_samples.resize(fixture_samples.len() + TARGET_RATE as usize, 0.0);
    let (fixture_noise_floor_dbfs, fixture_segments) = segment(&fixture_samples);
    let fixture_segment = fixture_segments
        .first()
        .context("Earshot did not detect the generated speech fixture")?;
    let segment_start = fixture_segment.start_ms as usize * TARGET_RATE as usize / 1_000;
    let segment_end = fixture_segment.emitted_ms as usize * TARGET_RATE as usize / 1_000;
    let utterance = &fixture_samples
        [segment_start.min(fixture_samples.len())..segment_end.min(fixture_samples.len())];

    let client = openai_client(&key)?;
    let mut transcription_trials = Vec::with_capacity(args.trials);
    for trial in 0..args.trials {
        let encode_started = Instant::now();
        let wav = encode_wav(scratch.path(), utterance, trial)?;
        let encode_ms = encode_started.elapsed().as_millis();
        let request_started = Instant::now();
        let response = transcribe(&client, &model, wav).await?;
        let request_ms = request_started.elapsed().as_millis();
        let text = response
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let languages = response
            .get("languages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|language| language.get("code").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let projected = u128::from(HANGOVER_FRAMES as u64 * FRAME_MS) + encode_ms + request_ms;
        println!(
            "trial {}: request={} ms, projected end→transcript={} ms, text={text:?}",
            trial + 1,
            request_ms,
            projected
        );
        transcription_trials.push(TranscriptionTrial {
            request_ms,
            encode_ms,
            projected_end_to_transcript_ms: projected,
            text,
            languages,
        });
    }

    let request_times = transcription_trials
        .iter()
        .map(|trial| trial.request_ms)
        .collect::<Vec<_>>();
    let projected_times = transcription_trials
        .iter()
        .map(|trial| trial.projected_end_to_transcript_ms)
        .collect::<Vec<_>>();
    let projected_550ms = transcription_trials
        .iter()
        .map(|trial| 550 + trial.encode_ms + trial.request_ms)
        .collect::<Vec<_>>();
    let speculative_350ms = transcription_trials
        .iter()
        .map(|trial| 350 + trial.encode_ms + trial.request_ms)
        .collect::<Vec<_>>();
    let request_p50_ms = percentile(&request_times, 0.50);
    let request_p95_ms = percentile(&request_times, 0.95);
    let projected_end_to_transcript_p50_ms = percentile(&projected_times, 0.50);
    let projected_end_to_transcript_p95_ms = percentile(&projected_times, 0.95);
    let projected_550ms_p50_ms = percentile(&projected_550ms, 0.50);
    let projected_550ms_p95_ms = percentile(&projected_550ms, 0.95);
    let speculative_350ms_p50_ms = percentile(&speculative_350ms, 0.50);
    let speculative_350ms_p95_ms = percentile(&speculative_350ms, 0.95);
    let transcripts_match = transcription_trials
        .iter()
        .all(|trial| has_expected_terms(&trial.text));

    let checks = vec![
        check(
            "audio callback had no ring overflow",
            capture.overflows == 0,
            format!("overflows={}", capture.overflows),
        ),
        check(
            "room capture contained real microphone samples",
            room_16k.len() >= TARGET_RATE as usize * args.room_seconds as usize * 9 / 10
                && room_peak_dbfs > -90.0,
            format!(
                "samples={}, peak={room_peak_dbfs:.1} dBFS, rms={room_rms_dbfs:.1} dBFS",
                room_16k.len()
            ),
        ),
        check(
            "normal-room sample produced no false upload",
            room_segments.is_empty(),
            format!(
                "segments={}, rate={false_triggers_per_minute:.2}/min",
                room_segments.len()
            ),
        ),
        check(
            "Earshot found the speech fixture",
            !fixture_segments.is_empty(),
            format!("segments={}", fixture_segments.len()),
        ),
        check(
            "fixture length represents a normal utterance",
            (3_000..=6_000).contains(&fixture_duration_ms),
            format!("duration={fixture_duration_ms} ms"),
        ),
        check(
            "GPT-Transcribe preserved the expected terms",
            transcripts_match,
            transcription_trials
                .iter()
                .map(|trial| trial.text.as_str())
                .collect::<Vec<_>>()
                .join(" | "),
        ),
        check(
            "projected end-to-transcript p50 met 1.5 s",
            projected_end_to_transcript_p50_ms < 1_500,
            format!("p50={projected_end_to_transcript_p50_ms} ms"),
        ),
        check(
            "projected end-to-transcript p95 met 2.5 s",
            projected_end_to_transcript_p95_ms < 2_500,
            format!("p95={projected_end_to_transcript_p95_ms} ms"),
        ),
    ];
    let passed = checks.iter().all(|check| check.passed);
    for failed in checks.iter().filter(|check| !check.passed) {
        eprintln!("FAIL {}: {}", failed.name, failed.detail);
    }

    let report = Report {
        passed,
        input_device: capture.device,
        input_sample_rate: capture.sample_rate,
        input_channels: capture.channels,
        room_seconds: args.room_seconds,
        room_overflows: capture.overflows,
        room_noise_floor_dbfs,
        room_sample_count: room_16k.len(),
        room_peak_dbfs,
        room_rms_dbfs,
        room_segments,
        false_triggers_per_minute,
        room_vad_cpu_ms,
        fixture_text: DEFAULT_FIXTURE.into(),
        fixture_duration_ms,
        fixture_noise_floor_dbfs,
        fixture_segments,
        transcription_trials,
        request_p50_ms,
        request_p95_ms,
        projected_end_to_transcript_p50_ms,
        projected_end_to_transcript_p95_ms,
        projected_550ms_p50_ms,
        projected_550ms_p95_ms,
        speculative_350ms_p50_ms,
        speculative_350ms_p95_ms,
        checks,
    };
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("S4 voice spike report → {}", report_path.display());
    if !passed {
        bail!("S4 acceptance checks failed");
    }
    Ok(())
}

fn parse_args() -> anyhow::Result<Args> {
    let mut room_seconds = 30;
    let mut trials = 5;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--room-seconds" => {
                index += 1;
                room_seconds = args
                    .get(index)
                    .context("--room-seconds needs a value")?
                    .parse()?;
            }
            "--trials" => {
                index += 1;
                trials = args.get(index).context("--trials needs a value")?.parse()?;
            }
            other => bail!("unknown argument {other:?}"),
        }
        index += 1;
    }
    if room_seconds == 0 || trials == 0 {
        bail!("room seconds and trials must be positive");
    }
    Ok(Args {
        room_seconds,
        trials,
    })
}

fn load_dotenv() {
    let Ok(text) = fs::read_to_string(".env") else {
        return;
    };
    for line in text.lines() {
        if let Some((name, value)) = line.split_once('=') {
            let name = name.trim();
            if !name.is_empty() && !name.starts_with('#') && std::env::var(name).is_err() {
                // Single-threaded at startup: nothing else reads the environment yet.
                unsafe { std::env::set_var(name, value.trim().trim_matches('"')) };
            }
        }
    }
}

fn capture_room(seconds: u64) -> anyhow::Result<Capture> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("no default input device")?;
    let supported = device.default_input_config()?;
    let sample_rate = supported.sample_rate();
    let channels = supported.channels();
    let config: StreamConfig = supported.clone().into();
    let capacity = sample_rate as usize * seconds as usize + sample_rate as usize;
    let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(capacity);
    let overflow_count = Arc::new(AtomicU64::new(0));
    let stream_error = Arc::new(Mutex::new(None::<String>));
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_stream_f32(
            &device,
            &config,
            producer,
            Arc::clone(&overflow_count),
            Arc::clone(&stream_error),
        )?,
        SampleFormat::I16 => build_stream_i16(
            &device,
            &config,
            producer,
            Arc::clone(&overflow_count),
            Arc::clone(&stream_error),
        )?,
        format => bail!("unsupported default input sample format {format}"),
    };
    stream.play()?;
    std::thread::sleep(Duration::from_secs(seconds));
    drop(stream);
    if let Some(error) = stream_error.lock().expect("stream error mutex").take() {
        bail!("input stream failed: {error}");
    }
    let mut samples = Vec::with_capacity(consumer.slots());
    while let Ok(sample) = consumer.pop() {
        samples.push(sample);
    }
    Ok(Capture {
        device: device.id()?.to_string(),
        sample_rate,
        channels,
        samples,
        overflows: overflow_count.load(Ordering::Relaxed),
    })
}

fn build_stream_f32(
    device: &cpal::Device,
    config: &StreamConfig,
    mut producer: rtrb::Producer<f32>,
    overflows: Arc<AtomicU64>,
    error: Arc<Mutex<Option<String>>>,
) -> anyhow::Result<Stream> {
    let channels = usize::from(config.channels);
    Ok(device.build_input_stream(
        *config,
        move |data: &[f32], _| {
            for frame in data.chunks_exact(channels) {
                let mono = frame.iter().sum::<f32>() / channels as f32;
                if producer.push(mono).is_err() {
                    overflows.fetch_add(1, Ordering::Relaxed);
                }
            }
        },
        move |stream_error| {
            *error.lock().expect("stream error mutex") = Some(stream_error.to_string());
        },
        None,
    )?)
}

fn build_stream_i16(
    device: &cpal::Device,
    config: &StreamConfig,
    mut producer: rtrb::Producer<f32>,
    overflows: Arc<AtomicU64>,
    error: Arc<Mutex<Option<String>>>,
) -> anyhow::Result<Stream> {
    let channels = usize::from(config.channels);
    Ok(device.build_input_stream(
        *config,
        move |data: &[i16], _| {
            for frame in data.chunks_exact(channels) {
                let mono = frame.iter().map(|sample| f32::from(*sample)).sum::<f32>()
                    / channels as f32
                    / 32_768.0;
                if producer.push(mono).is_err() {
                    overflows.fetch_add(1, Ordering::Relaxed);
                }
            }
        },
        move |stream_error| {
            *error.lock().expect("stream error mutex") = Some(stream_error.to_string());
        },
        None,
    )?)
}

fn resample_mono(samples: &[f32], input_rate: u32) -> anyhow::Result<Vec<f32>> {
    if input_rate == TARGET_RATE {
        return Ok(samples.to_vec());
    }
    let input = InterleavedOwned::new_from(samples.to_vec(), 1, samples.len())?;
    let mut resampler = Fft::<f32>::new(
        input_rate as usize,
        TARGET_RATE as usize,
        1_024,
        1,
        FixedSync::Both,
    )?;
    Ok(resampler
        .process_all(&input, samples.len(), None)?
        .take_data())
}

fn segment(samples: &[f32]) -> (f32, Vec<Segment>) {
    let calibration_frames =
        (TARGET_RATE as usize / FRAME_SAMPLES).min(samples.len() / FRAME_SAMPLES);
    let mut calibration_rms = samples
        .chunks_exact(FRAME_SAMPLES)
        .take(calibration_frames)
        .map(rms)
        .collect::<Vec<_>>();
    calibration_rms.sort_by(f32::total_cmp);
    let floor = calibration_rms
        .get(calibration_rms.len().saturating_div(10))
        .copied()
        .unwrap_or(0.000_01)
        .max(0.000_01);
    let gate = floor * 10.0_f32.powf(6.0 / 20.0);
    let floor_dbfs = 20.0 * floor.log10();

    let mut detector = Detector::default_boxed();
    let mut start_run = 0usize;
    let mut active_start = None::<usize>;
    let mut speech_frames = 0usize;
    let mut last_speech = 0usize;

    let mut peak = 0.0_f32;
    let mut segments = Vec::new();
    for (frame_index, frame) in samples.chunks_exact(FRAME_SAMPLES).enumerate() {
        let score = detector.predict_f32(frame);
        let is_speech = score >= 0.5 && rms(frame) >= gate;
        if active_start.is_none() {
            start_run = if is_speech { start_run + 1 } else { 0 };
            if start_run >= START_FRAMES {
                let first = frame_index + 1 - START_FRAMES;
                active_start = Some(first.saturating_sub(PRE_ROLL_FRAMES));
                speech_frames = START_FRAMES;
                last_speech = frame_index;
                peak = score;
            }
            continue;
        }
        if is_speech {
            speech_frames += 1;
            last_speech = frame_index;
            peak = peak.max(score);
        }
        if frame_index.saturating_sub(last_speech) >= HANGOVER_FRAMES {
            if speech_frames >= MIN_SPEECH_FRAMES {
                let start = active_start.expect("active start exists");
                segments.push(Segment {
                    start_ms: start as u64 * FRAME_MS,
                    speech_end_ms: (last_speech as u64 + 1) * FRAME_MS,
                    emitted_ms: (frame_index as u64 + 1) * FRAME_MS,
                    duration_ms: (frame_index + 1 - start) as u64 * FRAME_MS,
                    speech_frames,
                    peak_score: peak,
                });
            }
            active_start = None;
            start_run = 0;
            speech_frames = 0;
            peak = 0.0;
        }
    }
    (floor_dbfs, segments)
}

fn rms(frame: &[f32]) -> f32 {
    (frame.iter().map(|sample| sample * sample).sum::<f32>() / frame.len() as f32).sqrt()
}
fn dbfs(amplitude: f32) -> f32 {
    20.0 * amplitude.max(0.000_01).log10()
}

fn synthesize_fixture(dir: &Path, text: &str) -> anyhow::Result<PathBuf> {
    let aiff = dir.join("fixture.aiff");
    let wav = dir.join("fixture.wav");
    let say = std::process::Command::new("/usr/bin/say")
        .args(["-v", "Samantha", "-r", "175", "-o"])
        .arg(&aiff)
        .arg(text)
        .status()?;
    if !say.success() {
        bail!("macOS say failed with {say}");
    }
    let convert = std::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
        .arg(&aiff)
        .arg(&wav)
        .status()?;
    if !convert.success() {
        bail!("afconvert failed with {convert}");
    }
    Ok(wav)
}

fn read_mono_wav(path: &Path) -> anyhow::Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != TARGET_RATE || spec.bits_per_sample != 16 {
        bail!("unexpected fixture format: {spec:?}");
    }
    reader
        .samples::<i16>()
        .map(|sample| {
            sample
                .map(|sample| f32::from(sample) / 32_768.0)
                .map_err(Into::into)
        })
        .collect()
}

fn encode_wav(dir: &Path, samples: &[f32], trial: usize) -> anyhow::Result<Vec<u8>> {
    let path = dir.join(format!("utterance-{trial}.wav"));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec)?;
    for sample in samples {
        writer.write_sample((sample.clamp(-1.0, 1.0) * 32_767.0) as i16)?;
    }
    writer.finalize()?;
    Ok(fs::read(path)?)
}

fn openai_client(key: &str) -> anyhow::Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    let mut authorization = HeaderValue::from_str(&format!("Bearer {key}"))?;
    authorization.set_sensitive(true);
    headers.insert(AUTHORIZATION, authorization);
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(8))
        .build()?)
}

async fn transcribe(client: &reqwest::Client, model: &str, wav: Vec<u8>) -> anyhow::Result<Value> {
    let audio = Part::bytes(wav)
        .file_name("utterance.wav")
        .mime_str("audio/wav")?;
    let form = Form::new()
        .part("file", audio)
        .text("model", model.to_owned())
        .text("response_format", "json")
        .text(
            "prompt",
            "Spoken requests to a desktop assistant named Stark.",
        )
        .text("keywords[]", "Stark")
        .text("keywords[]", "Hypercanvas")
        .text("languages[]", "en");
    let response = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .multipart(form)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        bail!("transcription failed ({status}): {body}");
    }
    Ok(serde_json::from_str(&body).context("invalid transcription JSON")?)
}

fn percentile(values: &[u128], quantile: f64) -> u128 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    sorted[index]
}

fn has_expected_terms(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("stark") && lower.contains("hypercanvas") && lower.contains("autumn")
}

fn check(name: &'static str, passed: bool, detail: String) -> Check {
    Check {
        name,
        passed,
        detail,
    }
}
