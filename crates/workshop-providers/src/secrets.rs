//! Secret storage: the OS keyring, and an in-memory store for tests and headless CI.
//!
//! There is deliberately no file-backed store. Workshop does not create a plaintext `secrets.json`.

use std::collections::HashMap;
use std::sync::Mutex;

/// Service name under which Workshop stores its own credentials. Only Workshop-created entries
/// are ever read; other applications' keychain items are never touched.
pub const KEYRING_SERVICE: &str = "dev.workshop.providers";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("operating-system keyring unavailable: {0}")]
    Unavailable(String),
    #[error("keyring operation failed for {provider}: {detail}")]
    Failed { provider: String, detail: String },
}

pub trait SecretStore: Send + Sync {
    fn get(&self, provider: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, provider: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, provider: &str) -> Result<(), SecretError>;
    /// Human-readable backend name for the review screen ("OS keyring", "memory").
    fn backend_name(&self) -> &'static str;
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
    /// A store whose entries are namespaced to one Workshop home, so two homes on the same
    /// machine do not share credentials.
    pub fn for_home(home_tag: &str) -> Self {
        Self {
            service: format!("{KEYRING_SERVICE}.{home_tag}"),
        }
    }

    fn entry(&self, provider: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, provider).map_err(|e| match e {
            keyring::Error::NoDefaultStore => SecretError::Unavailable(e.to_string()),
            other => SecretError::Failed {
                provider: provider.to_string(),
                detail: other.to_string(),
            },
        })
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, provider: &str) -> Result<Option<String>, SecretError> {
        match self.entry(provider)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Failed {
                provider: provider.to_string(),
                detail: e.to_string(),
            }),
        }
    }

    fn set(&self, provider: &str, value: &str) -> Result<(), SecretError> {
        self.entry(provider)?
            .set_password(value)
            .map_err(|e| SecretError::Failed {
                provider: provider.to_string(),
                detail: e.to_string(),
            })
    }

    fn delete(&self, provider: &str) -> Result<(), SecretError> {
        match self.entry(provider)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Failed {
                provider: provider.to_string(),
                detail: e.to_string(),
            }),
        }
    }

    fn backend_name(&self) -> &'static str {
        "OS keyring"
    }
}

/// In-memory store for tests, CI, and `--no-keyring` sessions (process lifetime only).
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<String, String>>,
    fail_next_set: Mutex<bool>,
}

impl MemorySecretStore {
    /// Make the next `set` fail (rollback tests).
    pub fn fail_next_set(&self) {
        *self.fail_next_set.lock().expect("memory store lock") = true;
    }

    pub fn len(&self) -> usize {
        self.values.lock().expect("memory store lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, provider: &str) -> Result<Option<String>, SecretError> {
        Ok(self
            .values
            .lock()
            .expect("memory store lock")
            .get(provider)
            .cloned())
    }

    fn set(&self, provider: &str, value: &str) -> Result<(), SecretError> {
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
        Ok(())
    }

    fn delete(&self, provider: &str) -> Result<(), SecretError> {
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
