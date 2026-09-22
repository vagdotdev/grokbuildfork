//! `voice-engine` — Workshop's speech-to-text helper process.
//!
//! Spawned by the TUI on the first `/voice` of a process (never at startup), it loads the pinned
//! Whisper model once, prints `ready`, then serves utterances over the framed protocol in
//! [`protocol`]. It stays warm between utterances and exits on its own after `--idle-timeout-secs`
//! without one, on `quit`, or when stdin closes (parent gone).
//!
//! ```text
//! voice-engine --model PATH [--language CODE] [--threads N] [--idle-timeout-secs 300]
//!              [--no-gpu] [--window-secs 8] [--interim-ms 500] [--probe-language en]
//! voice-engine --probe --model PATH     load, probe-decode, print `ready`, exit
//! voice-engine --version
//! ```
//!
//! Language: a concrete Whisper code is passed through; `auto` (or none) lets whisper.cpp detect
//! it on the first decode of each utterance, after which the detected code is pinned for that
//! utterance (detection costs an extra encoder pass). The literal string `auto` never reaches
//! whisper. The startup probe always uses a concrete code (`--probe-language`, default `en`) so
//! `probe_ms` measures one interim decode, which is what the model-tier decision needs.
//!
//! Exit codes: 0 normal, 2 usage/protocol error, 3 the model could not be loaded (the parent
//! treats 3 as "re-verify and re-download the model").

mod decode;
mod protocol;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use decode::{Engine, Mode, SAMPLE_RATE, pcm16le_to_f32};
use protocol::{Frame, Message, emit};

const VERSION_LINE: &str = concat!(
    "voice-engine ",
    env!("CARGO_PKG_VERSION"),
    " (whisper.cpp via whisper-rs)"
);

struct Args {
    model: Option<PathBuf>,
    language: Option<String>,
    probe_language: String,
    threads: usize,
    idle_timeout: Duration,
    use_gpu: bool,
    window_secs: f32,
    interim_every: Duration,
    probe_only: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: voice-engine --model PATH [--language CODE] [--threads N] [--idle-timeout-secs S] \
         [--no-gpu] [--window-secs S] [--interim-ms MS] [--probe-language CODE] [--probe]\n       \
         voice-engine --version"
    );
    std::process::exit(2)
}

fn concrete_language(raw: Option<String>) -> Option<String> {
    raw.map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty() && l != "auto")
}

fn parse_args() -> Args {
    let mut args = Args {
        model: None,
        language: None,
        probe_language: "en".to_owned(),
        threads: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 8),
        idle_timeout: Duration::from_secs(300),
        use_gpu: true,
        window_secs: 8.0,
        interim_every: Duration::from_millis(500),
        probe_only: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" | "-V" => {
                println!(
                    "{VERSION_LINE} whisper.cpp {}",
                    whisper_rs::WHISPER_CPP_VERSION
                );
                std::process::exit(0);
            }
            "--model" => args.model = it.next().map(PathBuf::from),
            "--language" => args.language = concrete_language(it.next()),
            "--probe-language" => {
                args.probe_language =
                    concrete_language(it.next()).unwrap_or_else(|| "en".to_owned())
            }
            "--threads" => {
                args.threads = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage())
            }
            "--idle-timeout-secs" => {
                let secs: u64 = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage());
                args.idle_timeout = Duration::from_secs(secs);
            }
            "--no-gpu" => args.use_gpu = false,
            "--window-secs" => {
                args.window_secs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage())
            }
            "--interim-ms" => {
                let ms: u64 = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage());
                args.interim_every = Duration::from_millis(ms);
            }
            "--probe" => args.probe_only = true,
            "--help" | "-h" => usage(),
            _ => usage(),
        }
    }
    args
}

/// Auto-detected languages are pinned for interims only once this much audio backs the guess;
/// one second of a quiet opening is not enough (base misread German as Arabic on it).
const DETECT_PIN_SAMPLES: usize = SAMPLE_RATE * 3;

struct Utterance {
    /// Concrete code requested by the parent; `None` = auto-detect.
    language: Option<String>,
    /// Language pinned for interim decodes after enough audio (auto mode only). The final decode
    /// always re-detects over the whole utterance, so a wrong early guess cannot poison it.
    detected: Option<String>,
    samples: Vec<f32>,
    decoded_upto: usize,
    last_decode_end: Instant,
    last_partial: String,
}

enum Inbound {
    Frame(Frame),
    Eof,
    Error(String),
}

fn main() {
    let args = parse_args();
    let Some(model) = args.model.clone() else {
        usage()
    };
    let model_name = model
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();

    let (engine, load_time) = match Engine::load(&model, args.use_gpu, args.threads) {
        Ok(v) => v,
        Err(message) => {
            let _ = emit(&Message::Error { message: &message });
            std::process::exit(3);
        }
    };
    let mut state = match engine.create_state() {
        Ok(s) => s,
        Err(message) => {
            let _ = emit(&Message::Error { message: &message });
            std::process::exit(3);
        }
    };

    // One-second probe with a concrete language: proves the weights decode and measures what one
    // interim decode costs here. The parent uses `probe_ms` to step down a model tier; this
    // process itself never swaps models. Slow is also said once on stderr.
    let probe = vec![0f32; SAMPLE_RATE];
    let probe_time = match engine.decode(
        &mut state,
        &probe,
        Some(&args.probe_language),
        Mode::Interim,
    ) {
        Ok(d) => d.elapsed,
        Err(message) => {
            let _ = emit(&Message::Error { message: &message });
            std::process::exit(3);
        }
    };
    if probe_time > Duration::from_secs(1) {
        eprintln!(
            "voice-engine: this machine decoded a 1 s probe in {} ms; live dictation with {model_name} will lag",
            probe_time.as_millis()
        );
    }
    // whisper.cpp lists its compiled backends ("METAL = 1", "CUDA = 1", …); CPU-only builds report
    // none even when a GPU was requested.
    let system_info = whisper_rs::print_system_info();
    let gpu = engine.gpu()
        && ["METAL = 1", "CUDA = 1", "VULKAN = 1", "SYCL = 1", "HIP = 1"]
            .iter()
            .any(|b| system_info.contains(b));
    if emit(&Message::Ready {
        model: &model_name,
        load_ms: load_time.as_millis() as u64,
        probe_ms: probe_time.as_millis() as u64,
        gpu,
    })
    .is_err()
    {
        std::process::exit(0);
    }
    if args.probe_only {
        return;
    }

    let (tx, rx) = mpsc::channel::<Inbound>();
    std::thread::Builder::new()
        .name("stdin-frames".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            loop {
                match protocol::read_frame(&mut stdin) {
                    Ok(Some(frame)) => {
                        if tx.send(Inbound::Frame(frame)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(Inbound::Eof);
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(Inbound::Error(e.to_string()));
                        return;
                    }
                }
            }
        })
        .expect("spawn stdin reader");

    let window_samples = (args.window_secs.max(1.0) * SAMPLE_RATE as f32) as usize;
    let mut session: Option<Utterance> = None;
    let mut last_activity = Instant::now();
    let default_language = args.language.clone();

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Inbound::Frame(Frame::Audio(bytes))) => {
                if let Some(u) = session.as_mut() {
                    pcm16le_to_f32(&bytes, &mut u.samples);
                }
                last_activity = Instant::now();
            }
            Ok(Inbound::Frame(Frame::Start(opts))) => {
                // A start while an utterance is open discards it: the parent aborted that press.
                session = Some(Utterance {
                    language: concrete_language(opts.language).or_else(|| default_language.clone()),
                    detected: None,
                    samples: Vec::with_capacity(SAMPLE_RATE * 30),
                    decoded_upto: 0,
                    last_decode_end: Instant::now() - args.interim_every,
                    last_partial: String::new(),
                });
                last_activity = Instant::now();
            }
            Ok(Inbound::Frame(Frame::Stop)) => {
                if let Some(u) = session.take() {
                    if !finalize(&engine, &mut state, &u) {
                        std::process::exit(0);
                    }
                } else if emit(&Message::Final {
                    text: "",
                    decode_ms: 0,
                    language: None,
                })
                .is_err()
                {
                    std::process::exit(0);
                }
                last_activity = Instant::now();
            }
            Ok(Inbound::Frame(Frame::Quit)) | Ok(Inbound::Eof) => {
                if let Some(u) = session.take() {
                    finalize(&engine, &mut state, &u);
                }
                return;
            }
            Ok(Inbound::Error(message)) => {
                let _ = emit(&Message::Error {
                    message: &format!("protocol: {message}"),
                });
                std::process::exit(2);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        if let Some(u) = session.as_mut() {
            let due = u.samples.len() >= SAMPLE_RATE
                && u.samples.len() > u.decoded_upto
                && u.last_decode_end.elapsed() >= args.interim_every;
            if due {
                let start = u.samples.len().saturating_sub(window_samples);
                let lang = u.language.as_deref().or(u.detected.as_deref());
                match engine.decode(&mut state, &u.samples[start..], lang, Mode::Interim) {
                    Ok(d) => {
                        if u.language.is_none()
                            && u.detected.is_none()
                            && u.samples.len() >= DETECT_PIN_SAMPLES
                        {
                            u.detected = d.language.clone();
                        }
                        if !d.text.is_empty() && d.text != u.last_partial {
                            if emit(&Message::Partial {
                                text: &d.text,
                                decode_ms: d.elapsed.as_millis() as u64,
                            })
                            .is_err()
                            {
                                std::process::exit(0);
                            }
                            u.last_partial = d.text;
                        }
                    }
                    Err(message) => {
                        eprintln!("voice-engine: interim decode failed: {message}");
                    }
                }
                u.decoded_upto = u.samples.len();
                u.last_decode_end = Instant::now();
            }
        }
        // Warm window over (or a parent that stopped feeding an open utterance): give the RAM
        // back. Silent by design. Live dictation always has audio frames arriving.
        if last_activity.elapsed() >= args.idle_timeout {
            return;
        }
    }
}

/// Full-utterance decode and `final` emit. Returns false when stdout is gone.
fn finalize(engine: &Engine, state: &mut whisper_rs::WhisperState, u: &Utterance) -> bool {
    // Under ~300 ms cannot hold a word; skip the decode rather than invite a hallucination.
    if u.samples.len() < SAMPLE_RATE * 3 / 10 {
        return emit(&Message::Final {
            text: "",
            decode_ms: 0,
            language: None,
        })
        .is_ok();
    }
    match engine.decode(state, &u.samples, u.language.as_deref(), Mode::Final) {
        Ok(d) => emit(&Message::Final {
            text: &d.text,
            decode_ms: d.elapsed.as_millis() as u64,
            language: d.language.as_deref(),
        })
        .is_ok(),
        Err(message) => emit(&Message::Error { message: &message }).is_ok(),
    }
}
