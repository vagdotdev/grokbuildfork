//! Orchestrate locate → identify → status for one vendor or all four.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::identify::{Identity, identify};
use crate::locate::{DetectConfig, locate};
use crate::model::Vendor;
use crate::status::{LoginState, login_state};

/// A candidate that failed identity verification (for example a generic `agent` binary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorProbe {
    pub vendor: Vendor,
    /// The first candidate that passed identity verification.
    pub binary: Option<Identity>,
    /// Candidates that were found but are not this vendor's CLI.
    pub rejected: Vec<Rejected>,
    /// Official status result; `None` when nothing is installed or login was not checked.
    pub login: Option<LoginState>,
    /// Cursor only: the desktop app is present (presence check of well-known paths).
    pub app_present: bool,
    pub elapsed: Duration,
}

impl VendorProbe {
    pub fn installed(&self) -> bool {
        self.binary.is_some()
    }

    pub fn ready(&self) -> bool {
        self.installed() && matches!(self.login, Some(LoginState::LoggedIn))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub claude: VendorProbe,
    pub codex: VendorProbe,
    pub cursor: VendorProbe,
    pub opencode: VendorProbe,
    pub elapsed: Duration,
}

impl Probe {
    pub fn get(&self, vendor: Vendor) -> &VendorProbe {
        match vendor {
            Vendor::Claude => &self.claude,
            Vendor::Codex => &self.codex,
            Vendor::Cursor => &self.cursor,
            Vendor::OpenCode => &self.opencode,
        }
    }
}

fn cursor_app_present(cfg: &DetectConfig) -> bool {
    cfg.cursor_app_dirs().iter().any(|p| p.exists())
}

/// Probe one vendor: locate candidates, verify the first real one, then ask its status command.
pub fn probe_vendor(vendor: Vendor, cfg: &DetectConfig) -> VendorProbe {
    let started = Instant::now();
    let mut rejected = Vec::new();
    let mut binary = None;
    for candidate in locate(vendor, cfg) {
        match identify(vendor, &candidate.path, cfg) {
            Ok(id) => {
                binary = Some(id);
                break;
            }
            Err(e) => rejected.push(Rejected {
                path: candidate.path,
                reason: e.to_string(),
            }),
        }
    }
    let login = match (&binary, cfg.check_login) {
        (Some(id), true) => Some(login_state(vendor, &id.path, cfg)),
        _ => None,
    };
    VendorProbe {
        vendor,
        binary,
        rejected,
        login,
        app_present: vendor == Vendor::Cursor && cursor_app_present(cfg),
        elapsed: started.elapsed(),
    }
}

/// Probe all four vendors concurrently.
pub fn probe_all(cfg: &DetectConfig) -> Probe {
    let started = Instant::now();
    let (claude, codex, cursor, opencode) = std::thread::scope(|s| {
        let a = s.spawn(|| probe_vendor(Vendor::Claude, cfg));
        let b = s.spawn(|| probe_vendor(Vendor::Codex, cfg));
        let c = s.spawn(|| probe_vendor(Vendor::Cursor, cfg));
        let d = s.spawn(|| probe_vendor(Vendor::OpenCode, cfg));
        (
            a.join().expect("claude probe thread"),
            b.join().expect("codex probe thread"),
            c.join().expect("cursor probe thread"),
            d.join().expect("opencode probe thread"),
        )
    });
    Probe {
        claude,
        codex,
        cursor,
        opencode,
        elapsed: started.elapsed(),
    }
}
