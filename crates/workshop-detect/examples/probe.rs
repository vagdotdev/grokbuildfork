//! Dogfood runner: probe the real machine and print what the Subscriptions tab would show.
//!
//! ```text
//! cargo run -p workshop-detect --example probe            # full probe, then each Ready rail's live models
//! cargo run -p workshop-detect --example probe -- --presence-only
//! cargo run -p workshop-detect --example probe -- --models  # also ask every installed CLI for its models, signed in or not
//! ```
//!
//! `--env KEY=VALUE` (repeatable) adds a variable to the children's environment, for fake CLIs
//! (`scripts/no-theft-fs-audit.sh`); credential-like names are refused like everywhere else.

use std::ffi::OsString;
use std::time::{Duration, Instant};

use workshop_detect::{
    DetectConfig, ModelsCache, Rail, RailModels, Refresh, Vendor, probe_all, rails, rails_models,
    subscription_models,
};

fn main() {
    let presence_only = std::env::args().any(|a| a == "--presence-only");
    let ask_models = std::env::args().any(|a| a == "--models");
    let args: Vec<String> = std::env::args().collect();
    let extra_env = args
        .windows(2)
        .filter(|w| w[0] == "--env")
        .filter_map(|w| w[1].split_once('='))
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect();
    let cfg = DetectConfig {
        check_login: !presence_only,
        extra_env,
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

    if ask_models {
        println!("\n# Model lists straight from each installed CLI (signed in or not)");
        for rail in Rail::ALL {
            let Some(id) = &probe.get(rail.vendor()).binary else {
                println!("[{:<6}] not installed", rail.display_name());
                continue;
            };
            let started = Instant::now();
            let answer = subscription_models(rail, &id.path, &cfg);
            let took = started.elapsed();
            match answer {
                Ok(list) => println!(
                    "[{:<6}] {} models in {took:?}{}: {}  account={}",
                    rail.display_name(),
                    list.models.len(),
                    if list.documented_aliases {
                        " (documented aliases)"
                    } else {
                        ""
                    },
                    list.models
                        .iter()
                        .map(|m| format!("{}{}", m.id, if m.is_default { "*" } else { "" }))
                        .collect::<Vec<_>>()
                        .join(" "),
                    serde_json::to_string(&list.account).unwrap_or_default()
                ),
                Err(e) => println!("[{:<6}] error in {took:?}: {e}", rail.display_name()),
            }
        }
    }

    // A throwaway cache: this runner must not touch a real Workshop home.
    let cache_dir =
        std::env::temp_dir().join(format!("workshop-detect-probe-{}", std::process::id()));
    let cache = ModelsCache::new(&cache_dir);
    let refresh = if presence_only {
        Refresh::CacheOnly
    } else {
        Refresh::Live {
            max_age: Duration::ZERO,
        }
    };
    let [claude, codex, cursor] = rails_models(&probe, &cfg, &cache, refresh);
    let _ = std::fs::remove_dir_all(&cache_dir);
    let rails = rails(&probe, |rail| match rail {
        Rail::Claude => claude.clone(),
        Rail::Codex => codex.clone(),
        Rail::Cursor => cursor.clone(),
    });

    println!("\n# Subscriptions tab");
    for rail in &rails {
        println!(
            "[{:<6}] pill={:<9} installed={:<5} connect={:<5} models={} {}{}",
            rail.rail.display_name(),
            rail.pill.label(),
            rail.installed,
            rail.show_connect,
            rail.models.len(),
            rail.empty_copy
                .map(|c| format!("copy={c:?} "))
                .unwrap_or_default(),
            match &rail.subscription {
                RailModels::Failed { reason } => format!("reason={reason:?}"),
                _ => String::new(),
            }
        );
    }

    println!("\n# JSON");
    println!(
        "{}",
        serde_json::to_string_pretty(&probe).unwrap_or_default()
    );
}
