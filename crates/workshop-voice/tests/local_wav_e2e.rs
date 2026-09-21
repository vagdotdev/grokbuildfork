//! End-to-end: a replayed WAV "microphone" → the provider-neutral pipeline → local whisper →
//! the same `VoiceEvent`s the pager consumes. Needs the `base.en` model (142 MB), which is
//! fetched into `models_dir()` on first run exactly like the product does, so it is `#[ignore]`d:
//!
//! ```bash
//! WORKSHOP_HOME=/tmp/workshop-home CC=gcc CXX=g++ \
//!   cargo test -p workshop-voice --release --test local_wav_e2e -- --ignored --nocapture
//! ```

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use workshop_voice::backend::SttSessionOptions;
use workshop_voice::eval::wer;
use workshop_voice::local::models::{self, WhisperModel};
use workshop_voice::{
    LocalOptions, LocalWhisperBackend, PcmReplayCapture, PipelineDeps, PipelineOptions,
    VoiceCommand, VoiceEvent, run_voice_pipeline,
};

const JFK_REFERENCE: &str = "And so, my fellow Americans, ask not what your country can do for you; ask what you can do for your country.";

fn fixture_pcm(name: &str) -> (Vec<u8>, f32) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let reader = hound::WavReader::open(&path).expect("fixture wav");
    let spec = reader.spec();
    assert_eq!((spec.sample_rate, spec.channels), (16_000, 1));
    let samples: Vec<i16> = reader.into_samples::<i16>().map(|s| s.unwrap()).collect();
    let secs = samples.len() as f32 / 16_000.0;
    (samples.iter().flat_map(|s| s.to_le_bytes()).collect(), secs)
}

/// Drive the pipeline like the pager does: PttPress, hold through the clip plus a silent tail,
/// PttRelease, then collect events until the session ends.
async fn dictate(
    backend: Arc<LocalWhisperBackend>,
    pcm: Vec<u8>,
    hold: Duration,
) -> Vec<(Duration, VoiceEvent)> {
    let deps = PipelineDeps {
        backend,
        capture: Arc::new(PcmReplayCapture::new(pcm)),
        options: PipelineOptions {
            session: SttSessionOptions {
                endpointing_ms: 500,
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
    let mut events = Vec::new();
    let mut released_at: Option<Instant> = None;
    let mut last_activity = Instant::now();
    loop {
        if released_at.is_none() && t0.elapsed() >= hold {
            cmd_tx.send(VoiceCommand::PttRelease).await.unwrap();
            released_at = Some(Instant::now());
            last_activity = Instant::now();
        }
        match tokio::time::timeout(Duration::from_millis(200), event_rx.recv()).await {
            Ok(Some(ev)) => {
                last_activity = Instant::now();
                events.push((t0.elapsed(), ev));
            }
            Ok(None) => break,
            Err(_) => {}
        }
        // After release the session flushes and ends; a few quiet seconds means it is done.
        if released_at.is_some() && last_activity.elapsed() > Duration::from_secs(4) {
            break;
        }
    }
    cmd_tx.send(VoiceCommand::Shutdown).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), pipeline).await;
    events
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the 142 MB base.en model; run with --ignored"]
async fn jfk_dictation_commits_transcript_with_low_wer() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("workshop_voice=debug")
        .with_writer(std::io::stderr)
        .try_init();

    let models_dir = models::models_dir();
    let model = WhisperModel::BaseEn;
    let dl = Instant::now();
    models::ensure_model(model, &models_dir, None)
        .await
        .expect("base.en downloaded or present");
    eprintln!(
        "model {} ready in {} ms at {}",
        model.id(),
        dl.elapsed().as_millis(),
        models_dir.display()
    );

    let backend = Arc::new(LocalWhisperBackend::new(LocalOptions {
        models_dir,
        ..LocalOptions::new(model)
    }));
    let (pcm, audio_secs) = fixture_pcm("jfk.wav");
    let hold = Duration::from_secs_f32(audio_secs) + Duration::from_millis(2500);
    let events = dictate(backend, pcm, hold).await;

    for (t, ev) in &events {
        eprintln!("{:>6.2}s {ev:?}", t.as_secs_f32());
    }
    let interims = events
        .iter()
        .filter(|(_, e)| matches!(e, VoiceEvent::InterimTranscript { .. }))
        .count();
    let finals: Vec<(Duration, String)> = events
        .iter()
        .filter_map(|(t, e)| match e {
            VoiceEvent::UtteranceFinal { text } => Some((*t, text.clone())),
            _ => None,
        })
        .collect();
    let errors: Vec<_> = events
        .iter()
        .filter(|(_, e)| matches!(e, VoiceEvent::Error { .. }))
        .collect();

    assert!(errors.is_empty(), "voice errors: {errors:?}");
    assert!(interims >= 1, "expected live interims while speaking");
    assert!(
        !finals.is_empty(),
        "expected at least one committed utterance"
    );

    let committed = finals
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let error_rate = wer(JFK_REFERENCE, &committed);
    let (first_final_at, _) = &finals[0];
    let commit_latency = first_final_at.as_secs_f32() - audio_secs;
    eprintln!(
        "committed={committed:?}\nWER={error_rate:.3} interims={interims} finals={} first-final-after-audio-end={commit_latency:.2}s",
        finals.len()
    );
    assert!(
        error_rate <= 0.15,
        "WER {error_rate:.3} too high: {committed:?}"
    );
    // Endpointing (0.5 s) + one final decode of ~11 s audio on this CPU; generous bound for CI noise.
    assert!(
        commit_latency < 8.0,
        "final commit took {commit_latency:.2}s after the audio ended"
    );
}
