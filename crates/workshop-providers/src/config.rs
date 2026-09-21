//! The connections file: non-secret metadata about configured providers, written atomically with
//! owner-only permissions. Secrets never go here.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::manifest::{CredentialSource, ProviderClass};

pub const CONNECTIONS_FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRecord {
    pub class: ProviderClass,
    pub credential: CredentialSource,
    /// Hosts this connection's credential may be sent to (copied from the manifest at save time).
    pub allowed_hosts: Vec<String>,
    /// Unix seconds.
    pub saved_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionsFile {
    pub version: u32,
    #[serde(default)]
    pub connections: BTreeMap<String, ConnectionRecord>,
}

impl Default for ConnectionsFile {
    fn default() -> Self {
        Self {
            version: CONNECTIONS_FILE_VERSION,
            connections: BTreeMap::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path}: unsupported connections file version {version}")]
    Version { path: PathBuf, version: u32 },
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> ConfigError + '_ {
    move |source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Create `dir` (and parents); on Unix, strip any group/other bits (never widen the owner's).
pub fn ensure_private_dir(dir: &Path) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir)
            .map_err(io_err(dir))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode & 0o700))
                .map_err(io_err(dir))?;
        }
    }
    Ok(())
}

fn open_private(path: &Path, truncate: bool) -> Result<File, ConfigError> {
    let mut options = OpenOptions::new();
    options
        .create(true)
        .read(true)
        .write(true)
        .truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(io_err(path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `mode` applies only on create; tighten an existing file too.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(io_err(path))?;
    }
    Ok(file)
}

/// Write `contents` to `path` atomically: temp file in the same directory, fsync, rename. The
/// result is owner-read/write only. A crash leaves either the old file or the new one, never a
/// partial write.
pub fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<(), ConfigError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_dir(dir)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".into());
    let temp = dir.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = open_private(&temp, true)?;
        file.write_all(contents).map_err(io_err(&temp))?;
        file.sync_all().map_err(io_err(&temp))?;
        drop(file);
        std::fs::rename(&temp, path).map_err(io_err(path))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(io_err(path))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Read the connections file; a missing file is an empty one.
pub fn read_connections(path: &Path) -> Result<ConnectionsFile, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ConnectionsFile::default()),
        Err(e) => return Err(io_err(path)(e)),
    };
    let file: ConnectionsFile =
        serde_json::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if file.version != CONNECTIONS_FILE_VERSION {
        return Err(ConfigError::Version {
            path: path.to_path_buf(),
            version: file.version,
        });
    }
    Ok(file)
}

pub fn write_connections(path: &Path, file: &ConnectionsFile) -> Result<(), ConfigError> {
    let mut contents = serde_json::to_vec_pretty(file).expect("connections file serializes");
    contents.push(b'\n');
    atomic_write_private(path, &contents)
}

/// Run `op` while holding an exclusive advisory lock on `<path>.lock`, so two Workshop processes
/// cannot interleave a read-modify-write.
pub fn with_lock<T>(
    path: &Path,
    op: impl FnOnce() -> Result<T, ConfigError>,
) -> Result<T, ConfigError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_dir(dir)?;
    let lock_path = dir.join(format!(
        ".{}.lock",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let lock = open_private(&lock_path, false)?;
    FileExt::lock_exclusive(&lock).map_err(io_err(&lock_path))?;
    let result = op();
    let _ = FileExt::unlock(&lock);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_is_private_and_leaves_no_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("home").join("connections.json");
        atomic_write_private(&path, b"{}\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(entries.len(), 1, "no temp file left behind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        // Overwrite keeps the file whole.
        atomic_write_private(&path, b"{\"version\":1}\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"version\":1}\n");
    }

    #[cfg(unix)]
    #[test]
    fn existing_loose_file_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("connections.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        atomic_write_private(&path, b"{}\n").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn missing_file_reads_as_empty_and_bad_version_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("connections.json");
        assert_eq!(read_connections(&path).unwrap(), ConnectionsFile::default());
        std::fs::write(&path, r#"{"version": 99, "connections": {}}"#).unwrap();
        assert!(matches!(
            read_connections(&path),
            Err(ConfigError::Version { version: 99, .. })
        ));
    }
}
