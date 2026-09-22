//! Secret storage: the OS keyring first; an owner-only file under the Workshop home only when no
//! keyring is available (headless Linux without Secret Service, CI); an in-memory store for tests.
//!
//! Workshop only ever reads secrets it wrote itself. Other applications' credential files,
//! keychain items, and databases are never opened.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::manifest::CredentialSource;

/// Service name under which Workshop stores its own credentials.
pub const KEYRING_SERVICE: &str = "dev.workshop.providers";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("operating-system keyring unavailable: {0}")]
    Unavailable(String),
    #[error("keyring operation failed for {provider}: {detail}")]
    Failed { provider: String, detail: String },
    #[error("secret file {path}: {detail}")]
    File { path: PathBuf, detail: String },
}

pub trait SecretStore: Send + Sync {
    fn get(&self, provider: &str) -> Result<Option<String>, SecretError>;
    /// Store the secret and report which backend now holds it.
    fn set(&self, provider: &str, value: &str) -> Result<CredentialSource, SecretError>;
    fn delete(&self, provider: &str) -> Result<(), SecretError>;
    /// Human-readable backend name for the review screen.
    fn backend_name(&self) -> &'static str;
}

/// `$WORKSHOP_HOME`, else `<home>/.workshop`.
pub fn workshop_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("WORKSHOP_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    xai_dirs::home_dir().map(|h| h.join(".workshop"))
}

/// OS keyring (macOS Keychain, Windows Credential Manager, Secret Service on Linux).
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self {
            service: KEYRING_SERVICE.to_string(),
        }
    }
}

impl KeyringSecretStore {
    /// A store whose entries are namespaced to one Workshop home.
    pub fn for_home(home_tag: &str) -> Self {
        Self {
            service: format!("{KEYRING_SERVICE}.{home_tag}"),
        }
    }

    fn entry(&self, provider: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, provider).map_err(|e| match e {
            keyring::Error::NoDefaultStore
            | keyring::Error::NoStorageAccess(_)
            | keyring::Error::PlatformFailure(_) => SecretError::Unavailable(e.to_string()),
            other => SecretError::Failed {
                provider: provider.to_string(),
                detail: other.to_string(),
            },
        })
    }

    fn classify(provider: &str, e: keyring::Error) -> SecretError {
        match e {
            keyring::Error::NoDefaultStore
            | keyring::Error::NoStorageAccess(_)
            | keyring::Error::PlatformFailure(_) => SecretError::Unavailable(e.to_string()),
            other => SecretError::Failed {
                provider: provider.to_string(),
                detail: other.to_string(),
            },
        }
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, provider: &str) -> Result<Option<String>, SecretError> {
        match self.entry(provider)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Self::classify(provider, e)),
        }
    }

    fn set(&self, provider: &str, value: &str) -> Result<CredentialSource, SecretError> {
        self.entry(provider)?
            .set_password(value)
            .map(|()| CredentialSource::Keyring)
            .map_err(|e| Self::classify(provider, e))
    }

    fn delete(&self, provider: &str) -> Result<(), SecretError> {
        match self.entry(provider)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Self::classify(provider, e)),
        }
    }

    fn backend_name(&self) -> &'static str {
        "OS keyring"
    }
}

/// Owner-only files under `<dir>/<provider>.secret` (dir 0700, files 0600, atomic writes).
#[derive(Debug, Clone)]
pub struct FileSecretStore {
    dir: PathBuf,
}

impl FileSecretStore {
    /// Normally `<workshop home>/secrets`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, provider: &str) -> PathBuf {
        self.dir.join(format!("{provider}.secret"))
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, provider: &str) -> Result<Option<String>, SecretError> {
        let path = self.path(provider);
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(s.trim_end_matches(['\n', '\r']).to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SecretError::File {
                path,
                detail: e.to_string(),
            }),
        }
    }

    fn set(&self, provider: &str, value: &str) -> Result<CredentialSource, SecretError> {
        let path = self.path(provider);
        crate::config::atomic_write_private(&path, format!("{value}\n").as_bytes())
            .map(|()| CredentialSource::File)
            .map_err(|e| SecretError::File {
                path,
                detail: e.to_string(),
            })
    }

    fn delete(&self, provider: &str) -> Result<(), SecretError> {
        let path = self.path(provider);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SecretError::File {
                path,
                detail: e.to_string(),
            }),
        }
    }

    fn backend_name(&self) -> &'static str {
        "file (0600, Workshop home)"
    }
}

/// Keyring first; the file store only when the keyring is unavailable. Reads check both so a
/// secret saved while the keyring was down is still found once it comes back.
pub struct LayeredSecretStore {
    keyring: Box<dyn SecretStore>,
    file: FileSecretStore,
}

impl LayeredSecretStore {
    pub fn new(keyring: Box<dyn SecretStore>, file: FileSecretStore) -> Self {
        Self { keyring, file }
    }

    /// The production store for `workshop_home`.
    pub fn for_workshop_home(home: &Path) -> Self {
        let tag = home.to_string_lossy();
        let digest = sha2::Sha256::digest(tag.as_bytes());
        let short: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
        Self::new(
            Box::new(KeyringSecretStore::for_home(&short)),
            FileSecretStore::new(home.join("secrets")),
        )
    }
}

use sha2::Digest as _;

impl SecretStore for LayeredSecretStore {
    fn get(&self, provider: &str) -> Result<Option<String>, SecretError> {
        match self.keyring.get(provider) {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) | Err(SecretError::Unavailable(_)) => self.file.get(provider),
            Err(e) => Err(e),
        }
    }

    fn set(&self, provider: &str, value: &str) -> Result<CredentialSource, SecretError> {
        match self.keyring.set(provider, value) {
            Ok(source) => {
                // A stale file copy must not shadow or outlive the keyring entry.
                let _ = self.file.delete(provider);
                Ok(source)
            }
            Err(SecretError::Unavailable(_)) => self.file.set(provider, value),
            Err(e) => Err(e),
        }
    }

    fn delete(&self, provider: &str) -> Result<(), SecretError> {
        let k = match self.keyring.delete(provider) {
            Ok(()) | Err(SecretError::Unavailable(_)) => Ok(()),
            Err(e) => Err(e),
        };
        self.file.delete(provider)?;
        k
    }

    fn backend_name(&self) -> &'static str {
        "OS keyring, file fallback"
    }
}

/// In-memory store for tests, CI, and `--no-keyring` sessions (process lifetime only).
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<String, String>>,
    fail_next_set: Mutex<bool>,
    unavailable: Mutex<bool>,
}

impl MemorySecretStore {
    /// Make the next `set` fail (rollback tests).
    pub fn fail_next_set(&self) {
        *self.fail_next_set.lock().expect("memory store lock") = true;
    }

    /// Behave like a machine with no keyring (fallback tests).
    pub fn set_unavailable(&self, unavailable: bool) {
        *self.unavailable.lock().expect("memory store lock") = unavailable;
    }

    pub fn len(&self) -> usize {
        self.values.lock().expect("memory store lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn check_available(&self) -> Result<(), SecretError> {
        if *self.unavailable.lock().expect("memory store lock") {
            return Err(SecretError::Unavailable(
                "no keyring on this machine (simulated)".into(),
            ));
        }
        Ok(())
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, provider: &str) -> Result<Option<String>, SecretError> {
        self.check_available()?;
        Ok(self
            .values
            .lock()
            .expect("memory store lock")
            .get(provider)
            .cloned())
    }

    fn set(&self, provider: &str, value: &str) -> Result<CredentialSource, SecretError> {
        self.check_available()?;
        let mut fail = self.fail_next_set.lock().expect("memory store lock");
        if *fail {
            *fail = false;
            return Err(SecretError::Failed {
                provider: provider.to_string(),
                detail: "injected failure".into(),
            });
        }
        self.values
            .lock()
            .expect("memory store lock")
            .insert(provider.to_string(), value.to_string());
        Ok(CredentialSource::Keyring)
    }

    fn delete(&self, provider: &str) -> Result<(), SecretError> {
        self.check_available()?;
        self.values
            .lock()
            .expect("memory store lock")
            .remove(provider);
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "memory (not persisted)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_is_owner_only_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(tmp.path().join("home/secrets"));
        assert_eq!(store.get("openai").unwrap(), None);
        assert_eq!(store.set("openai", "sk-1").unwrap(), CredentialSource::File);
        assert_eq!(store.get("openai").unwrap().as_deref(), Some("sk-1"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(store.dir().join("openai.secret"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        store.delete("openai").unwrap();
        assert_eq!(store.get("openai").unwrap(), None);
        store.delete("openai").unwrap();
    }

    #[test]
    fn layered_store_falls_back_to_file_only_when_keyring_is_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let mem = std::sync::Arc::new(MemorySecretStore::default());
        struct Shared(std::sync::Arc<MemorySecretStore>);
        impl SecretStore for Shared {
            fn get(&self, p: &str) -> Result<Option<String>, SecretError> {
                self.0.get(p)
            }
            fn set(&self, p: &str, v: &str) -> Result<CredentialSource, SecretError> {
                self.0.set(p, v)
            }
            fn delete(&self, p: &str) -> Result<(), SecretError> {
                self.0.delete(p)
            }
            fn backend_name(&self) -> &'static str {
                "mem"
            }
        }
        let layered = LayeredSecretStore::new(
            Box::new(Shared(mem.clone())),
            FileSecretStore::new(tmp.path().join("secrets")),
        );

        assert_eq!(
            layered.set("openai", "k1").unwrap(),
            CredentialSource::Keyring
        );
        assert!(
            !tmp.path().join("secrets/openai.secret").exists(),
            "keyring worked; no file written"
        );

        mem.set_unavailable(true);
        assert_eq!(layered.set("nvidia", "k2").unwrap(), CredentialSource::File);
        assert!(tmp.path().join("secrets/nvidia.secret").exists());
        assert_eq!(layered.get("nvidia").unwrap().as_deref(), Some("k2"));

        // Keyring back: reads find both; a re-save moves the secret out of the file.
        mem.set_unavailable(false);
        assert_eq!(layered.get("openai").unwrap().as_deref(), Some("k1"));
        assert_eq!(layered.get("nvidia").unwrap().as_deref(), Some("k2"));
        assert_eq!(
            layered.set("nvidia", "k3").unwrap(),
            CredentialSource::Keyring
        );
        assert!(!tmp.path().join("secrets/nvidia.secret").exists());
        assert_eq!(layered.get("nvidia").unwrap().as_deref(), Some("k3"));

        layered.delete("nvidia").unwrap();
        layered.delete("openai").unwrap();
        assert_eq!(layered.get("openai").unwrap(), None);
        assert!(mem.is_empty());
    }

    #[test]
    fn workshop_home_honours_the_env_override() {
        // SAFETY: unique to this test.
        unsafe { std::env::set_var("WORKSHOP_HOME", "/tmp/ws-home-test") };
        assert_eq!(workshop_home(), Some(PathBuf::from("/tmp/ws-home-test")));
        unsafe { std::env::remove_var("WORKSHOP_HOME") };
        assert!(workshop_home().is_some_and(|p| p.ends_with(".workshop")));
    }
}
