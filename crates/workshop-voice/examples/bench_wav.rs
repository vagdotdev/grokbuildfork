//! Measure local whisper on a 16 kHz mono WAV: batch decode cost per model/mode, and the
//! streaming session's event timeline (interim cadence, commit latency after end of speech).
//!
//! ```bash
//! WORKSHOP_HOME=/tmp/workshop-home cargo run -p workshop-voice --release --example bench_wav -- \
//!     --wav samples/jfk.wav --model base.en --model small.en \
//!     --reference "And so, my fellow Americans, ask not what your country can do for you; ask what you can do for your country."
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use workshop_voice::backend::SttSessionOptions;
use workshop_voice::eval::wer;
use workshop_voice::local::engine::{DecodeConfig, DecodeMode};
use workshop_voice::local::models::{self, WhisperModel};
use workshop_voice::{
    LocalOptions, LocalWhisperBackend, PcmReplayCapture, PipelineDeps, PipelineOptions,
    VoiceCommand, VoiceEvent, run_voice_pipeline,
};

struct Args {
    wav: String,
    models: Vec<WhisperModel>,
    reference: Option<String>,
    realtime: bool,
    beam: usize,
    audio_ctx: i32,
    endpointing_ms: u32,
    skip_stream: bool,
    skip_batch: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        wav: String::new(),
        models: Vec::new(),
        reference: None,
        realtime: true,
        beam: 5,
        audio_ctx: 768,
        endpointing_ms: 600,
        skip_stream: false,
        skip_batch: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--wav" => args.wav = it.next().unwrap_or_default(),
            "--model" => {
                let id = it.next().unwrap_or_default();
                if id == "all" {
                    args.models.extend(WhisperModel::ALL.iter().copied());
                } else {
                    args.models.push(
                        WhisperModel::parse(&id).unwrap_or_else(|| panic!("unknown model {id}")),
                    );
                }
            }
            "--reference" => args.reference = it.next(),
            "--fast" => args.realtime = false,
            "--beam" => args.beam = it.next().and_then(|v| v.parse().ok()).unwrap_or(5),
            "--audio-ctx" => args.audio_ctx = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--endpointing-ms" => {
                args.endpointing_ms = it.next().and_then(|v| v.parse().ok()).unwrap_or(600)
            }
            "--no-stream" => args.skip_stream = true,
            "--no-batch" => args.skip_batch = true,
            other => panic!("unknown arg {other}"),
        }
    }
    if args.wav.is_empty() {
        panic!("--wav is required");
    }
    if args.models.is_empty() {
        args.models.push(WhisperModel::BaseEn);
    }
    args
}

fn read_wav_pcm16(path: &str) -> (Vec<u8>, Vec<f32>, f32) {
    let reader = hound::WavReader::open(path).expect("open wav");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 16_000, "bench expects a 16 kHz WAV");
    assert_eq!(spec.channels, 1, "bench expects mono");
    let samples: Vec<i16> = match spec.sample_format {
        hound::SampleFormat::Int => reader.into_samples::<i16>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .map(|s| (s.unwrap() * 32767.0) as i16)
            .collect(),
    };
    let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let f32s: Vec<f32> = samples.iter().map(|s| *s as f32 / 32768.0).collect();
    let secs = samples.len() as f32 / 16_000.0;
    (pcm, f32s, secs)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,workshop_voice=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = parse_args();
    let (pcm, f32s, audio_secs) = read_wav_pcm16(&args.wav);
    let models_dir = models::models_dir();
    println!(
        "wav={} audio={audio_secs:.2}s models_dir={} cpus={}",
        args.wav,
        models_dir.display(),
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    );

    for model in &args.models {
        let model = *model;
        let path = models::ensure_model(model, &models_dir, None)
            .await
            .expect("model available");
        println!(
            "\n=== {} ({} MB) ===",
            model.id(),
            model.size_bytes() / 1_000_000
        );

        let opts = LocalOptions {
            models_dir: models_dir.clone(),
            final_beam: args.beam,
            interim_audio_ctx: args.audio_ctx,
            min_endpointing_ms: args.endpointing_ms,
            ..LocalOptions::new(model)
        };
        let backend = Arc::new(LocalWhisperBackend::new(opts.clone()));

        let load_started = Instant::now();
        let engine = backend.engine().await.expect("engine");
        println!(
            "load: {} ms ({})",
            load_started.elapsed().as_millis(),
            path.display()
        );

        if !args.skip_batch {
            let mut state = engine.create_state().expect("state");
            let cfg = DecodeConfig {
                final_beam: args.beam,
                ..DecodeConfig::default()
            };
            for (label, mode, beam, audio_ctx) in [
                ("interim greedy full-ctx", DecodeMode::Interim, 1usize, 0),
                (
                    "interim greedy audio-ctx",
                    DecodeMode::Interim,
                    1usize,
                    args.audio_ctx,
                ),
                ("final greedy+fallback", DecodeMode::Final, 1usize, 0),
                ("final beam", DecodeMode::Final, args.beam, 0),
            ] {
                let cfg = DecodeConfig {
                    final_beam: beam,
                    interim_audio_ctx: audio_ctx,
                    ..cfg.clone()
                };
                // Warm-up run is skipped on purpose: first-call cost is what a user sees.
                let d = engine
                    .decode(&mut state, &f32s, &cfg, mode)
                    .expect("decode");
                let rtf = d.elapsed.as_secs_f32() / audio_secs;
                let wer_s = args
                    .reference
                    .as_deref()
                    .map(|r| format!(" wer={:.3}", wer(r, &d.text)))
                    .unwrap_or_default();
                println!(
                    "batch {label:<25} {:>6} ms  rtf={rtf:.2}{wer_s}\n      {:?}",
                    d.elapsed.as_millis(),
                    d.text
                );
            }
        }

        if args.skip_stream {
            continue;
        }
        // Streaming through the real pipeline with a replayed "microphone".
        let capture = Arc::new(PcmReplayCapture::new(pcm.clone()).realtime(args.realtime));
        let deps = PipelineDeps {
            backend: backend.clone(),
            capture,
            options: PipelineOptions {
                session: SttSessionOptions {
                    endpointing_ms: args.endpointing_ms,
                    ..SttSessionOptions::default()
                },
                ..PipelineOptions::default()
            },
        };
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
        let pipeline = tokio::spawn(run_voice_pipeline(deps, cmd_rx, event_tx));

        let t0 = Instant::now();
        cmd_tx.send(VoiceCommand::PttPress).await.unwrap();
        // Hold the "key" for the whole clip plus a tail so trailing silence endpoints the last utterance.
        let hold = if args.realtime {
            Duration::from_secs_f32(audio_secs) + Duration::from_millis(2500)
        } else {
            Duration::from_millis(3500)
        };
        let mut events: Vec<(Duration, VoiceEvent)> = Vec::new();
        let release_at = t0 + hold;
        let mut released = false;
        loop {
            let now = Instant::now();
            if !released && now >= release_at {
                cmd_tx.send(VoiceCommand::PttRelease).await.unwrap();
                released = true;
            }
            let wait = if released {
                Duration::from_millis(200)
            } else {
                release_at
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(200))
            };
            match tokio::time::timeout(wait, event_rx.recv()).await {
                Ok(Some(ev)) => events.push((t0.elapsed(), ev)),
                Ok(None) => break,
                Err(_) => {
                    // After release, a quiet gap means the session is over.
                    if released
                        && events
                            .last()
                            .map(|(t, _)| t0.elapsed() - *t > Duration::from_secs(8))
                            .unwrap_or(t0.elapsed() - hold > Duration::from_secs(8))
                    {
                        break;
                    }
                }
            }
        }
        cmd_tx.send(VoiceCommand::Shutdown).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), pipeline).await;

        println!(
            "stream ({}): {} events; audio ends at {audio_secs:.2}s; release at {:.2}s",
            if args.realtime {
                "realtime"
            } else {
                "fast replay"
            },
            events.len(),
            hold.as_secs_f32()
        );
        let mut committed = Vec::new();
        for (t, ev) in &events {
            match ev {
                VoiceEvent::InterimTranscript { text } => {
                    println!("  {:>7.2}s interim  {text:?}", t.as_secs_f32())
                }
                VoiceEvent::UtteranceFinal { text } => {
                    println!("  {:>7.2}s FINAL    {text:?}", t.as_secs_f32());
                    committed.push(text.clone());
                }
                VoiceEvent::Error { message, .. } => {
                    println!("  {:>7.2}s ERROR    {message}", t.as_secs_f32())
                }
            }
        }
        let joined = committed.join(" ");
        if let Some(r) = args.reference.as_deref() {
            println!("stream committed wer={:.3}: {joined:?}", wer(r, &joined));
        } else {
            println!("stream committed: {joined:?}");
        }
        if args.realtime
            && let Some((t, _)) = events
                .iter()
                .find(|(_, e)| matches!(e, VoiceEvent::UtteranceFinal { .. }))
        {
            println!(
                "first FINAL at {:.2}s (audio {audio_secs:.2}s)",
                t.as_secs_f32()
            );
        }
    }
}
