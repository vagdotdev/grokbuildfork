//! Workshop overlay: end-to-end dictation through the real pipeline with the local engine.
//!
//! Upstream's Linux capture spawns a system recorder from `PATH`; the test puts a fake `arecord`
//! there that streams a WAV at real-time pace, so `run_voice_pipeline` exercises the genuine
//! path: recorder subprocess → PCM pipe → `voice-engine` helper → `VoiceEvent`s. Needs the helper
//! and a model, so it is `#[ignore]`d:
//!
//! ```bash
//! WORKSHOP_VOICE_ENGINE=voice/engine/target/release/voice-engine \
//! WORKSHOP_VOICE_DIR=/tmp/workshop-home/voice \
//! FAKE_MIC_BIN_DIR=/tmp/fakemic FAKE_MIC_WAV=crates/workshop-voice/tests/fixtures/jfk.wav \
//! cargo test -p xai-grok-voice --test local_dictation -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use xai_grok_voice::{StaticVoiceAuth, VoiceCommand, VoiceConfig, VoiceEvent, run_voice_pipeline};

const JFK: &str = "And so, my fellow Americans, ask not what your country can do for you; ask what you can do for your country.";

fn normalize(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect()
        })
        .filter(|w: &String| !w.is_empty())
        .collect()
}

fn wer(reference: &str, hypothesis: &str) -> f32 {
    let r = normalize(reference);
    let h = normalize(hypothesis);
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, hw) in h.iter().enumerate() {
            let sub = prev[j] + usize::from(rw != hw);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[h.len()] as f32 / r.len().max(1) as f32
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs voice-engine, a model, and the fake recorder (see module docs)"]
async fn jfk_dictation_through_the_real_pipeline() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("xai_grok_voice=debug,workshop_voice=info,voice_engine=info")
        .with_writer(std::io::stderr)
        .try_init();
    let fake_dir = std::env::var("FAKE_MIC_BIN_DIR").expect("FAKE_MIC_BIN_DIR");
    assert!(std::env::var("FAKE_MIC_WAV").is_ok(), "FAKE_MIC_WAV");
    // The recorder is resolved from PATH by upstream's capture backend.
    // SAFETY: single-threaded setup before any task reads the environment.
    unsafe { std::env::set_var("PATH", fake_dir) };
    let wav_secs = {
        let r = hound::WavReader::open(std::env::var("FAKE_MIC_WAV").unwrap()).unwrap();
        r.duration() as f32 / r.spec().sample_rate as f32
    };

    let config = VoiceConfig::default();
    assert_eq!(config.provider, xai_grok_voice::VoiceProvider::Local);
    let auth = StaticVoiceAuth::shared("unused-by-the-local-provider").unwrap();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(32);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(128);
    let pipeline = tokio::spawn(run_voice_pipeline(config, auth, cmd_rx, event_tx));

    let t0 = Instant::now();
    cmd_tx.send(VoiceCommand::PttPress).await.unwrap();
    // The recorder starts streaming at once while the engine gets ready (model verify, load,
    // probe, possibly a tier step-down); upstream's pre-connect backlog keeps that audio. Release
    // (Esc / Enter) once the session is ready and the clip has fully played out.
    let mut events: Vec<(Duration, VoiceEvent)> = Vec::new();
    let mut released_at: Option<Instant> = None;
    let mut ready_at: Option<Instant> = None;
    loop {
        if released_at.is_none() {
            let clip_done = t0.elapsed() >= Duration::from_secs_f32(0.5 + wav_secs + 1.0);
            let session_ready = ready_at.is_some_and(|r| r.elapsed() >= Duration::from_secs(2));
            if (clip_done && session_ready) || t0.elapsed() > Duration::from_secs(240) {
                cmd_tx.send(VoiceCommand::PttRelease).await.unwrap();
                released_at = Some(Instant::now());
            }
        }
        match tokio::time::timeout(Duration::from_millis(200), event_rx.recv()).await {
            Ok(Some(ev)) => {
                let fatal = matches!(ev, VoiceEvent::Error { .. });
                if matches!(&ev, VoiceEvent::Status { text } if text.is_empty()) {
                    ready_at.get_or_insert(Instant::now());
                }
                events.push((t0.elapsed(), ev));
                if fatal {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {}
        }
        if let Some(r) = released_at
            && (events
                .iter()
                .any(|(_, e)| matches!(e, VoiceEvent::UtteranceFinal { .. }))
                || r.elapsed() > Duration::from_secs(150))
        {
            break;
        }
    }
    cmd_tx.send(VoiceCommand::Shutdown).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), pipeline).await;

    let mut first_interim = None;
    let mut final_text = None;
    let mut final_at = None;
    let mut statuses = Vec::new();
    for (t, ev) in &events {
        eprintln!("{:>7.2}s {ev:?}", t.as_secs_f32());
        match ev {
            VoiceEvent::Status { text } => statuses.push(text.clone()),
            VoiceEvent::InterimTranscript { .. } => {
                first_interim.get_or_insert(*t);
            }
            VoiceEvent::UtteranceFinal { text } => {
                final_text = Some(text.clone());
                final_at = Some(*t);
            }
            VoiceEvent::Error { message, .. } => panic!("voice error: {message}"),
        }
    }
    let final_text = final_text.expect("one committed utterance");
    let release_at = released_at.unwrap() - t0;
    let error_rate = wer(JFK, &final_text);
    eprintln!(
        "\nfirst interim at {:.2}s after press; final {:.2}s after release; WER {error_rate:.3}\nstatuses: {statuses:?}\nfinal: {final_text:?}",
        first_interim.map(|d| d.as_secs_f32()).unwrap_or(f32::NAN),
        (final_at.unwrap() - release_at).as_secs_f32()
    );
    assert!(first_interim.is_some(), "partials must show while speaking");
    assert!(
        statuses
            .iter()
            .any(|s| s.starts_with("Loading voice model"))
            && statuses.last().is_some_and(String::is_empty),
        "banner status must show the engine getting ready and then clear: {statuses:?}"
    );
    assert!(error_rate <= 0.15, "WER {error_rate:.3}: {final_text:?}");
}
