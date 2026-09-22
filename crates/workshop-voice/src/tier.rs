//! Automatic, silent model tiering (voice-spec §9.6).
//!
//! Three pinned tiers, fastest last: `turbo` (large-v3-turbo q5_0), `small`, `base`. The rule is
//! "the largest tier whose interim decode fits in about a second on this machine", biased toward
//! speed:
//!
//! - Apple Silicon (Metal) → `turbo`, no probe.
//! - Everything else is CPU-only in our builds. The installer downloads `base` (148 MB, also the
//!   floor every machine can run), times `voice-engine --probe` on it, predicts the bigger tiers
//!   from measured cost ratios, downloads the predicted tier and confirms it with a second probe.
//! - At `/voice`, every cold helper start reports `probe_ms`; over budget → step down one tier
//!   (same download/progress path) and persist the choice in `<voice dir>/model.selected`.
//! - `voice.model = "turbo" | "small" | "base"` in config forces a tier (undocumented).
//!
//! The installer mirrors this logic in POSIX sh (`scripts/install.sh`, `voice_pick_tier`).

use std::path::{Path, PathBuf};

use crate::manifest;

pub const SELECTION_FILE: &str = "model.selected";

/// Tier ids, largest first.
pub fn tiers() -> &'static [String] {
    &manifest::lock().tiers
}

pub fn is_tier(id: &str) -> bool {
    tiers().iter().any(|t| t == id)
}

/// The next faster (smaller) tier, if any.
pub fn next_lower(tier: &str) -> Option<&'static str> {
    let t = tiers();
    let idx = t.iter().position(|x| x == tier)?;
    t.get(idx + 1).map(String::as_str)
}

pub fn smallest() -> &'static str {
    tiers().last().map(String::as_str).unwrap_or("base")
}

pub fn interim_budget_ms() -> u64 {
    manifest::lock().selection.interim_budget_ms
}

pub fn within_budget(probe_ms: u64) -> bool {
    probe_ms <= interim_budget_ms()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Machine {
    pub apple_silicon: bool,
    pub cores: usize,
    pub ram_bytes: Option<u64>,
}

pub fn detect_machine() -> Machine {
    Machine {
        apple_silicon: cfg!(all(target_os = "macos", target_arch = "aarch64")),
        cores: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        ram_bytes: total_ram_bytes(),
    }
}

#[cfg(target_os = "linux")]
fn total_ram_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(not(target_os = "linux"))]
fn total_ram_bytes() -> Option<u64> {
    None
}

/// Tier to start from when nothing was measured yet (no selection file, no probe).
pub fn static_default(m: &Machine) -> &'static str {
    if m.apple_silicon {
        return "turbo";
    }
    if m.ram_bytes
        .is_some_and(|ram| ram < manifest::lock().selection.min_ram_bytes_for_probe)
    {
        return smallest();
    }
    if m.cores >= 8 { "small" } else { "base" }
}

/// Predict the largest tier that fits the interim budget from a probe of `base`.
pub fn predict_from_base_probe(base_probe_ms: u64) -> &'static str {
    let sel = &manifest::lock().selection;
    let budget = sel.interim_budget_ms as f64;
    let base = base_probe_ms as f64;
    if base * sel.probe_ratio_turbo_over_base <= budget {
        "turbo"
    } else if base * sel.probe_ratio_small_over_base <= budget {
        "small"
    } else {
        "base"
    }
}

pub fn selection_path(dir: &Path) -> PathBuf {
    dir.join(SELECTION_FILE)
}

/// The persisted choice, if it names a known tier.
pub fn read_selection(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(selection_path(dir)).ok()?;
    let tier = raw.trim().to_owned();
    is_tier(&tier).then_some(tier)
}

pub fn write_selection(dir: &Path, tier: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{SELECTION_FILE}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, format!("{tier}\n"))?;
    std::fs::rename(&tmp, selection_path(dir))
}

/// Which tier `/voice` should use right now.
///
/// 1. `override_tier` (`voice.model` in config) when it names a tier;
/// 2. the persisted selection;
/// 3. the largest tier whose file is already on disk (a copied install);
/// 4. the static default for this machine.
pub fn resolve(dir: &Path, override_tier: Option<&str>, machine: &Machine) -> String {
    if let Some(t) = override_tier.map(str::trim).filter(|t| is_tier(t)) {
        return t.to_owned();
    }
    if let Some(t) = read_selection(dir) {
        return t;
    }
    for t in tiers() {
        if let Some(store) = crate::store::ModelStore::for_tier(dir, t)
            && store.quick_status().is_ready()
        {
            return t.clone();
        }
    }
    static_default(machine).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(cores: usize, ram_gib: u64) -> Machine {
        Machine {
            apple_silicon: false,
            cores,
            ram_bytes: Some(ram_gib << 30),
        }
    }

    #[test]
    fn tier_order_and_stepping() {
        assert_eq!(tiers(), &["turbo", "small", "base"]);
        assert_eq!(next_lower("turbo"), Some("small"));
        assert_eq!(next_lower("small"), Some("base"));
        assert_eq!(next_lower("base"), None);
        assert_eq!(smallest(), "base");
        assert!(within_budget(1000));
        assert!(!within_budget(1001));
    }

    #[test]
    fn static_default_prefers_metal_then_cores_then_ram_floor() {
        let m1 = Machine {
            apple_silicon: true,
            cores: 8,
            ram_bytes: None,
        };
        assert_eq!(static_default(&m1), "turbo");
        assert_eq!(static_default(&cpu(16, 32)), "small");
        assert_eq!(static_default(&cpu(4, 16)), "base");
        assert_eq!(
            static_default(&cpu(16, 2)),
            "base",
            "under 3 GiB never probes"
        );
    }

    /// Ratios and budget from the lock file, applied to the VM this was measured on.
    #[test]
    fn prediction_from_base_probe_matches_measured_vm() {
        assert_eq!(predict_from_base_probe(383), "base"); // 4 vCPU Xeon: small would be ~1.5 s
        assert_eq!(predict_from_base_probe(250), "small"); // 250 * 3.9 = 975 ms
        assert_eq!(predict_from_base_probe(50), "turbo"); // 50 * 19.5 = 975 ms
        assert_eq!(predict_from_base_probe(52), "small");
    }

    #[test]
    fn resolve_order_override_then_file_then_disk_then_default() {
        let dir = tempfile::tempdir().unwrap();
        let m = cpu(4, 16);
        assert_eq!(resolve(dir.path(), None, &m), "base");
        assert_eq!(resolve(dir.path(), Some("turbo"), &m), "turbo");
        assert_eq!(
            resolve(dir.path(), Some("tiny"), &m),
            "base",
            "unknown override ignored"
        );
        // A present file wins over the static default
        let small = crate::store::ModelStore::for_tier(dir.path(), "small").unwrap();
        let f = std::fs::File::create(small.path()).unwrap();
        f.set_len(small.pin().size).unwrap();
        assert_eq!(resolve(dir.path(), None, &m), "small");
        // The persisted selection wins over what is on disk
        write_selection(dir.path(), "base").unwrap();
        assert_eq!(read_selection(dir.path()).as_deref(), Some("base"));
        assert_eq!(resolve(dir.path(), None, &m), "base");
        std::fs::write(selection_path(dir.path()), "garbage\n").unwrap();
        assert_eq!(read_selection(dir.path()), None);
    }
}
