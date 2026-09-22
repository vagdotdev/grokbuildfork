//! Dogfood runner: probe the real machine and print what the Subscriptions tab would show.
//!
//! ```text
//! cargo run -p workshop-detect --example probe            # full probe (status commands run)
//! cargo run -p workshop-detect --example probe -- --presence-only
//! ```

use workshop_detect::model::default_models;
use workshop_detect::{DetectConfig, Vendor, probe_all, rails};

fn main() {
    let presence_only = std::env::args().any(|a| a == "--presence-only");
    let cfg = DetectConfig {
        check_login: !presence_only,
        ..DetectConfig::default()
    };
    let probe = probe_all(&cfg);

    println!(
        "# workshop-detect probe ({})",
        if presence_only {
            "presence only"
        } else {
            "with official status commands"
        }
    );
    println!("# elapsed: {:?}", probe.elapsed);
    for vendor in Vendor::ALL {
        let vp = probe.get(vendor);
        match &vp.binary {
            Some(id) => println!(
                "{:<9} installed  {}  version={}",
                vendor.id(),
                id.path.display(),
                id.version
            ),
            None => println!("{:<9} not installed", vendor.id()),
        }
        for r in &vp.rejected {
            println!("{:<9} rejected   {}  ({})", "", r.path.display(), r.reason);
        }
        println!(
            "{:<9} login      {}",
            "",
            match &vp.login {
                Some(state) => serde_json::to_string(state).unwrap_or_default(),
                None => "not checked".to_string(),
            }
        );
    }

    println!("\n# Subscriptions tab");
    for rail in rails(&probe, default_models) {
        println!(
            "[{:<6}] pill={:<9} installed={:<5} connect={:<5} models={} {}",
            rail.rail.display_name(),
            rail.pill.label(),
            rail.installed,
            rail.show_connect,
            rail.models.len(),
            rail.empty_copy
                .map(|c| format!("copy={c:?}"))
                .unwrap_or_default()
        );
    }

    println!("\n# JSON");
    println!(
        "{}",
        serde_json::to_string_pretty(&probe).unwrap_or_default()
    );
}
