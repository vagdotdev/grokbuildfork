//! Open a local voice session the way the TUI does and print every status line and event.
//! Diagnostic for the tiering / self-heal path: `WORKSHOP_VOICE_ENGINE=… WORKSHOP_VOICE_DIR=… cargo run -p workshop-voice --example open_session`
use std::time::Instant;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "workshop_voice=debug".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let t0 = Instant::now();
    let opts = workshop_voice::OpenOptions {
        language: std::env::var("VOICE_LANGUAGE").ok(),
        tier_override: std::env::var("VOICE_TIER").ok(),
        ..Default::default()
    };
    let status = |text: String| println!("{:>7.2}s status: {text:?}", t0.elapsed().as_secs_f32());
    // Two presses in a row: the first pays model verify + helper start (+ any tier step-down),
    // the second reuses the warm helper.
    for press in 1..=2 {
        let started = Instant::now();
        match workshop_voice::open(&opts, &status).await {
            Ok((mut session, opened)) => {
                println!(
                    "{:>7.2}s press {press}: session open in {} ms — {opened:?}",
                    t0.elapsed().as_secs_f32(),
                    started.elapsed().as_millis()
                );
                session.finish_audio();
                while let Some(ev) = session.recv().await {
                    println!("{:>7.2}s event: {ev:?}", t0.elapsed().as_secs_f32());
                }
            }
            Err(e) => {
                println!("{:>7.2}s error: {e}", t0.elapsed().as_secs_f32());
                std::process::exit(1);
            }
        }
    }
    workshop_voice::engine::shutdown().await;
}
