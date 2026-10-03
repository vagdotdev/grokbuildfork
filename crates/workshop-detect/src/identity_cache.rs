//! Remember which binaries already passed [`crate::identify`], so a launch does not re-run
//! `--version` (and `--help`) on a CLI it verified last time.
//!
//! Each vendor CLI is a Node or Bun bundle whose `--version` is a full runtime start: 0.4–1 s of
//! CPU on a laptop, and OpenCode needs two of them. Every launch verifies the engine's binary
//! before `opencode serve` can start, and every picker snapshot verifies all four vendors, so
//! these probes were the largest fixed cost between `workshop` and a ready engine.
//!
//! An entry is keyed by the binary's path, size and modification time; any change to the file
//! misses the cache and runs the real probes again. Only identities that passed verification are
//! ever stored, in Workshop's own cache directory (`$WORKSHOP_HOME/catalog-cache/cli-identity.json`,
//! owner-only). Nothing of another application's is read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::identify::Identity;
use crate::model::Vendor;

/// The on-disk file: one entry per binary path.
#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    vendor: Vendor,
    version: String,
    len: u64,
    mtime_ns: u128,
}

/// Serializes the read-modify-write of the file within this process: the four vendors are
/// probed on parallel threads and each stores its own entry. (Two Workshop processes writing at
/// the same instant can still lose one entry to the other; that costs one re-probe later.)
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// The binary's fingerprint: `(len, mtime)` of the file at `path`, `None` when it cannot be
/// stat'ed (then nothing is cached or served).
fn fingerprint(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((meta.len(), mtime))
}

#[derive(Debug, Clone)]
pub struct IdentityCache {
    path: PathBuf,
}

impl IdentityCache {
    /// The cache file at `path` (created on the first store).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> File {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// The identity stored for `bin` as `vendor`, if the binary is byte-for-byte the one that
    /// was verified (same path, size and modification time).
    pub fn lookup(&self, vendor: Vendor, bin: &Path) -> Option<Identity> {
        let (len, mtime_ns) = fingerprint(bin)?;
        let file = self.read();
        let entry = file.entries.get(&bin.to_string_lossy().into_owned())?;
        (entry.vendor == vendor && entry.len == len && entry.mtime_ns == mtime_ns).then(|| {
            Identity {
                vendor,
                path: bin.to_path_buf(),
                version: entry.version.clone(),
            }
        })
    }

    /// Remember a verified identity (owner-only file, written to a temp name and renamed).
    pub fn store(&self, id: &Identity) {
        let Some((len, mtime_ns)) = fingerprint(&id.path) else {
            return;
        };
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.read();
        file.entries.insert(
            id.path.to_string_lossy().into_owned(),
            Entry {
                vendor: id.vendor,
                version: id.version.clone(),
                len,
                mtime_ns,
            },
        );
        self.write(&file);
    }

    /// Drop what is remembered for `bin`: the next detection runs the real probes. Called when a
    /// binary that passed verification then fails to run, so the repair path sees the failure.
    pub fn forget(&self, bin: &Path) {
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.read();
        if file
            .entries
            .remove(&bin.to_string_lossy().into_owned())
            .is_some()
        {
            self.write(&file);
        }
    }

    fn write(&self, file: &File) {
        let Ok(json) = serde_json::to_vec_pretty(file) else {
            return;
        };
        if let Some(dir) = self.path.parent()
            && std::fs::create_dir_all(dir).is_err()
        {
            return;
        }
        let tmp = self
            .path
            .with_extension(format!("json.tmp-{}", std::process::id()));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let written = opts.open(&tmp).and_then(|mut f| {
            use std::io::Write;
            f.write_all(&json)?;
            f.sync_all()
        });
        if written.is_err() || std::fs::rename(&tmp, &self.path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(path: &Path) -> Identity {
        Identity {
            vendor: Vendor::OpenCode,
            path: path.to_path_buf(),
            version: "1.18.31".into(),
        }
    }

    #[test]
    fn store_then_lookup_serves_the_same_binary_only() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("opencode");
        std::fs::write(&bin, "#!/bin/sh\necho 1.18.31\n").unwrap();
        let cache = IdentityCache::new(tmp.path().join("catalog-cache").join("cli-identity.json"));
        assert_eq!(cache.lookup(Vendor::OpenCode, &bin), None);

        cache.store(&identity(&bin));
        assert_eq!(cache.lookup(Vendor::OpenCode, &bin), Some(identity(&bin)));
        // The entry is for OpenCode; asking whether the same file is Claude runs the real probe.
        assert_eq!(cache.lookup(Vendor::Claude, &bin), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(cache.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_changed_binary_misses() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("opencode");
        std::fs::write(&bin, "v1").unwrap();
        let cache = IdentityCache::new(tmp.path().join("cli-identity.json"));
        cache.store(&identity(&bin));
        assert!(cache.lookup(Vendor::OpenCode, &bin).is_some());

        // A different size (an update in place) is a different binary.
        std::fs::write(&bin, "v2 longer").unwrap();
        assert_eq!(cache.lookup(Vendor::OpenCode, &bin), None);

        // Same size, different mtime: still a miss.
        std::fs::write(&bin, "v1").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::open(&bin)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(cache.lookup(Vendor::OpenCode, &bin), None);

        // A binary that is gone is never served from the cache.
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(cache.lookup(Vendor::OpenCode, &bin), None);
    }

    #[test]
    fn forget_drops_one_entry_and_garbage_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("opencode");
        let b = tmp.path().join("claude");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let cache = IdentityCache::new(tmp.path().join("cli-identity.json"));
        cache.store(&identity(&a));
        cache.store(&Identity {
            vendor: Vendor::Claude,
            path: b.clone(),
            version: "2.1.278".into(),
        });
        cache.forget(&a);
        assert_eq!(cache.lookup(Vendor::OpenCode, &a), None);
        assert_eq!(
            cache.lookup(Vendor::Claude, &b).map(|id| id.version),
            Some("2.1.278".into())
        );

        std::fs::write(cache.path(), "{not json").unwrap();
        assert_eq!(cache.lookup(Vendor::Claude, &b), None);
        // A store over garbage starts a fresh file.
        cache.store(&identity(&a));
        assert!(cache.lookup(Vendor::OpenCode, &a).is_some());
    }
}
