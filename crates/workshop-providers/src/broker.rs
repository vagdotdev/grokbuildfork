//! The credential broker: the only component that hands a Workshop-owned secret to a request.
//!
//! * Each saved credential is bound to a provider id, its auth header, and the manifest's host
//!   allowlist. [`CredentialHandle::authorize`] refuses any other host, so provider A's key can
//!   never travel to provider B (canary test in `tests/isolation.rs`).
//! * Saving is two-phase: keyring first, then the atomic connections file; a failure rolls the
//!   keyring back to its previous state.
//! * Environment keys are reported by presence only until the user chooses "Use without saving"
//!   or "Save to Workshop"; [`CredentialBroker::env_key_present`] never returns the value.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use url::Url;

use crate::config::{
    ConfigError, ConnectionRecord, ConnectionsFile, read_connections, with_lock, write_connections,
};
use crate::manifest::{AuthHeader, CredentialSource, ProviderManifest, manifest};
use crate::secrets::{SecretError, SecretStore};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("unknown provider {0}")]
    UnknownProvider(String),
    #[error("invalid provider id {0:?}")]
    InvalidProviderId(String),
    #[error("API key cannot be empty")]
    EmptyKey,
    #[error("{provider} has no credential configured")]
    NoCredential { provider: String },
    #[error("refusing to send the {provider} credential to {host}: not in its host allowlist")]
    HostNotAllowed { provider: String, host: String },
    #[error(
        "plaintext http is only allowed on loopback ({0}); confirm a custom dev endpoint explicitly"
    )]
    PlaintextNotLoopback(String),
    #[error("{0} is not a valid URL")]
    BadUrl(String),
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("{error}; rolling back the keyring also failed: {rollback}")]
    Rollback { error: String, rollback: String },
}

/// Where a resolved credential comes from, without the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRef {
    /// Present in the process environment (value not read yet).
    Env {
        var: String,
    },
    /// Saved in the OS keyring by the user.
    Keyring,
    None,
}

/// A credential bound to one provider and its host allowlist. The value is only released through
/// [`CredentialHandle::authorize`] for an allowed URL.
#[derive(Clone)]
pub struct CredentialHandle {
    provider_id: String,
    auth: AuthHeader,
    allowed_hosts: Vec<String>,
    value: Option<String>,
}

impl std::fmt::Debug for CredentialHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialHandle")
            .field("provider_id", &self.provider_id)
            .field("auth", &self.auth)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("value", &self.value.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl CredentialHandle {
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn auth(&self) -> AuthHeader {
        self.auth
    }

    pub fn has_value(&self) -> bool {
        self.value.is_some()
    }

    /// Release the credential for a request to `url`, or refuse when the host is not allowed.
    pub fn authorize(&self, url: &str) -> Result<Option<&str>, ProviderError> {
        let parsed = Url::parse(url).map_err(|_| ProviderError::BadUrl(url.to_string()))?;
        let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
        if !self
            .allowed_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&host))
        {
            return Err(ProviderError::HostNotAllowed {
                provider: self.provider_id.clone(),
                host,
            });
        }
        validate_scheme(&parsed, &self.allowed_hosts, false)?;
        Ok(self.value.as_deref())
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Plaintext `http://` is only acceptable on loopback, or for a custom dev endpoint the user has
/// explicitly confirmed after seeing the resolved origin.
pub fn validate_scheme(
    url: &Url,
    _allowed_hosts: &[String],
    confirmed_dev_endpoint: bool,
) -> Result<(), ProviderError> {
    match url.scheme() {
        "https" => Ok(()),
        "http" => {
            let host = url.host_str().unwrap_or("");
            if is_loopback_host(host) || confirmed_dev_endpoint {
                Ok(())
            } else {
                Err(ProviderError::PlaintextNotLoopback(
                    url.origin().ascii_serialization(),
                ))
            }
        }
        other => Err(ProviderError::BadUrl(format!("{other}://"))),
    }
}

fn validate_provider_id(id: &str) -> Result<(), ProviderError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ProviderError::InvalidProviderId(id.to_string()));
    }
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct CredentialBroker {
    store: Arc<dyn SecretStore>,
    connections_path: PathBuf,
}

impl CredentialBroker {
    /// `connections_path` is normally `<workshop home>/connections.json`.
    pub fn new(store: Arc<dyn SecretStore>, connections_path: impl Into<PathBuf>) -> Self {
        Self {
            store,
            connections_path: connections_path.into(),
        }
    }

    pub fn connections_path(&self) -> &Path {
        &self.connections_path
    }

    pub fn secret_backend(&self) -> &'static str {
        self.store.backend_name()
    }

    fn manifest_for(&self, provider_id: &str) -> Result<ProviderManifest, ProviderError> {
        validate_provider_id(provider_id)?;
        manifest(provider_id).ok_or_else(|| ProviderError::UnknownProvider(provider_id.to_string()))
    }

    /// Presence-only check of the manifest's environment variable. The value is never returned.
    pub fn env_key_present(&self, provider_id: &str) -> Result<bool, ProviderError> {
        let m = self.manifest_for(provider_id)?;
        Ok(match m.credential {
            CredentialSource::Env { var } => std::env::var_os(&var).is_some_and(|v| !v.is_empty()),
            _ => false,
        })
    }

    pub fn connections(&self) -> Result<ConnectionsFile, ProviderError> {
        Ok(read_connections(&self.connections_path)?)
    }

    /// "Save to Workshop": store the key in the keyring, then record the connection atomically.
    pub fn save_api_key(&self, provider_id: &str, key: &str) -> Result<(), ProviderError> {
        let m = self.manifest_for(provider_id)?;
        if key.trim().is_empty() {
            return Err(ProviderError::EmptyKey);
        }
        if m.auth == AuthHeader::None {
            return Err(ProviderError::UnknownProvider(format!(
                "{provider_id} takes no credential"
            )));
        }
        let key = key.trim();
        let path = self.connections_path.clone();
        let store = Arc::clone(&self.store);
        let record = ConnectionRecord {
            class: m.class,
            credential: CredentialSource::Keyring,
            allowed_hosts: m.allowed_hosts.iter().map(|h| h.to_string()).collect(),
            saved_at: now_secs(),
        };
        let provider = provider_id.to_string();
        with_lock(&path, || {
            // Read first so a broken connections file never leaves a half-saved keyring entry.
            let mut file = read_connections(&path)?;
            let previous = store.get(&provider).map_err(config_wrap)?;
            store.set(&provider, key).map_err(config_wrap)?;
            file.connections.insert(provider.clone(), record.clone());
            if let Err(error) = write_connections(&path, &file) {
                let rollback = match previous {
                    Some(v) => store.set(&provider, &v),
                    None => store.delete(&provider),
                };
                return Err(match rollback {
                    Ok(()) => error,
                    Err(rb) => config_wrap(SecretError::Failed {
                        provider: provider.clone(),
                        detail: format!("{error}; rollback failed: {rb}"),
                    }),
                });
            }
            Ok(())
        })
        .map_err(Into::into)
    }

    /// "Use without saving": record that this provider uses its environment variable. No value is
    /// read or stored.
    pub fn use_env_key(&self, provider_id: &str) -> Result<(), ProviderError> {
        let m = self.manifest_for(provider_id)?;
        let var = match &m.credential {
            CredentialSource::Env { var } => var.clone(),
            _ => {
                return Err(ProviderError::UnknownProvider(format!(
                    "{provider_id} has no env credential"
                )));
            }
        };
        let record = ConnectionRecord {
            class: m.class,
            credential: CredentialSource::Env { var },
            allowed_hosts: m.allowed_hosts.iter().map(|h| h.to_string()).collect(),
            saved_at: now_secs(),
        };
        let path = self.connections_path.clone();
        with_lock(&path, || {
            let mut file = read_connections(&path)?;
            file.connections
                .insert(provider_id.to_string(), record.clone());
            write_connections(&path, &file)
        })?;
        Ok(())
    }

    /// Record a Local connection (no credential).
    pub fn add_local(&self, provider_id: &str) -> Result<(), ProviderError> {
        let m = self.manifest_for(provider_id)?;
        if !m.is_local() {
            return Err(ProviderError::UnknownProvider(format!(
                "{provider_id} is not a Local provider"
            )));
        }
        let record = ConnectionRecord {
            class: m.class,
            credential: CredentialSource::None,
            allowed_hosts: m.allowed_hosts.iter().map(|h| h.to_string()).collect(),
            saved_at: now_secs(),
        };
        let path = self.connections_path.clone();
        with_lock(&path, || {
            let mut file = read_connections(&path)?;
            file.connections
                .insert(provider_id.to_string(), record.clone());
            write_connections(&path, &file)
        })?;
        Ok(())
    }

    /// `workshop auth logout <provider>` for Direct API: delete Workshop's own secret and record.
    /// Never touches another application's files.
    pub fn forget(&self, provider_id: &str) -> Result<bool, ProviderError> {
        validate_provider_id(provider_id)?;
        let path = self.connections_path.clone();
        let store = Arc::clone(&self.store);
        let provider = provider_id.to_string();
        let existed = with_lock(&path, || {
            let mut file = read_connections(&path)?;
            let had_record = file.connections.remove(&provider).is_some();
            let had_secret = store.get(&provider).map_err(config_wrap)?.is_some();
            store.delete(&provider).map_err(config_wrap)?;
            write_connections(&path, &file)?;
            Ok(had_record || had_secret)
        })?;
        Ok(existed)
    }

    /// Where the credential would come from, without reading it.
    pub fn credential_ref(&self, provider_id: &str) -> Result<CredentialRef, ProviderError> {
        let m = self.manifest_for(provider_id)?;
        let file = self.connections()?;
        Ok(
            match file.connections.get(provider_id).map(|r| &r.credential) {
                Some(CredentialSource::Keyring) => CredentialRef::Keyring,
                Some(CredentialSource::Env { var }) => CredentialRef::Env { var: var.clone() },
                Some(CredentialSource::None) => CredentialRef::None,
                None => match m.credential {
                    CredentialSource::Env { var } if std::env::var_os(&var).is_some() => {
                        CredentialRef::Env { var }
                    }
                    _ => CredentialRef::None,
                },
            },
        )
    }

    /// Resolve the credential for a request, bound to the provider's host allowlist.
    pub fn resolve(&self, provider_id: &str) -> Result<CredentialHandle, ProviderError> {
        let m = self.manifest_for(provider_id)?;
        let allowed_hosts: Vec<String> = m.allowed_hosts.iter().map(|h| h.to_string()).collect();
        let value =
            match (m.auth, self.credential_ref(provider_id)?) {
                (AuthHeader::None, _) => None,
                (_, CredentialRef::Keyring) => {
                    Some(self.store.get(provider_id)?.ok_or_else(|| {
                        ProviderError::NoCredential {
                            provider: provider_id.to_string(),
                        }
                    })?)
                }
                (_, CredentialRef::Env { var }) => Some(
                    std::env::var(&var)
                        .ok()
                        .filter(|v| !v.is_empty())
                        .ok_or_else(|| ProviderError::NoCredential {
                            provider: provider_id.to_string(),
                        })?,
                ),
                (_, CredentialRef::None) => {
                    return Err(ProviderError::NoCredential {
                        provider: provider_id.to_string(),
                    });
                }
            };
        Ok(CredentialHandle {
            provider_id: provider_id.to_string(),
            auth: m.auth,
            allowed_hosts,
            value,
        })
    }
}

/// Secret-store failures inside a locked config section surface as config errors so the lock
/// closure has one error type; the broker maps them back.
fn config_wrap(e: SecretError) -> ConfigError {
    ConfigError::Io {
        path: PathBuf::from("<keyring>"),
        source: std::io::Error::other(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecretStore;

    fn broker(tmp: &tempfile::TempDir) -> (CredentialBroker, Arc<MemorySecretStore>) {
        let store = Arc::new(MemorySecretStore::default());
        let broker = CredentialBroker::new(store.clone(), tmp.path().join("home/connections.json"));
        (broker, store)
    }

    #[test]
    fn save_stores_secret_in_keyring_and_metadata_in_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (broker, store) = broker(&tmp);
        broker.save_api_key("openai", " sk-test-123 ").unwrap();
        assert_eq!(store.get("openai").unwrap().as_deref(), Some("sk-test-123"));
        let text = std::fs::read_to_string(broker.connections_path()).unwrap();
        assert!(
            !text.contains("sk-test"),
            "secret must not be in the connections file: {text}"
        );
        assert!(text.contains(r#""source": "keyring""#));
        assert_eq!(
            broker.credential_ref("openai").unwrap(),
            CredentialRef::Keyring
        );
        let handle = broker.resolve("openai").unwrap();
        assert_eq!(
            handle
                .authorize("https://api.openai.com/v1/responses")
                .unwrap(),
            Some("sk-test-123")
        );
        assert!(format!("{handle:?}").contains("<redacted>"));
    }

    #[test]
    fn unreadable_config_never_touches_the_keyring() {
        let tmp = tempfile::tempdir().unwrap();
        let (broker, store) = broker(&tmp);
        broker.save_api_key("openai", "old-key").unwrap();
        // A directory where the file should be makes the read fail before any keyring write.
        std::fs::remove_file(broker.connections_path()).unwrap();
        std::fs::create_dir_all(broker.connections_path()).unwrap();
        assert!(broker.save_api_key("openai", "new-key").is_err());
        assert_eq!(store.get("openai").unwrap().as_deref(), Some("old-key"));
    }

    #[cfg(unix)]
    #[test]
    fn failed_config_write_rolls_back_the_keyring() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc_geteuid() } == 0 {
            eprintln!("skipped: read-only directories do not block root");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let (broker, store) = broker(&tmp);
        broker.save_api_key("openai", "old-key").unwrap();
        let dir = broker.connections_path().parent().unwrap().to_path_buf();
        // Read still works; creating the temp file for the atomic write does not.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = broker.save_api_key("openai", "new-key");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert_eq!(
            store.get("openai").unwrap().as_deref(),
            Some("old-key"),
            "keyring rolled back"
        );
        assert!(matches!(
            broker.credential_ref("openai").unwrap(),
            CredentialRef::Keyring
        ));
    }

    #[cfg(unix)]
    unsafe fn libc_geteuid() -> u32 {
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        unsafe { geteuid() }
    }

    #[test]
    fn keyring_failure_leaves_file_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let (broker, store) = broker(&tmp);
        store.fail_next_set();
        assert!(matches!(
            broker.save_api_key("anthropic", "k"),
            Err(ProviderError::Config(_))
        ));
        assert!(broker.connections().unwrap().connections.is_empty());
    }

    #[test]
    fn rejects_bad_input() {
        let tmp = tempfile::tempdir().unwrap();
        let (broker, _) = broker(&tmp);
        assert!(matches!(
            broker.save_api_key("openai", "  "),
            Err(ProviderError::EmptyKey)
        ));
        assert!(matches!(
            broker.save_api_key("../x", "k"),
            Err(ProviderError::InvalidProviderId(_))
        ));
        assert!(matches!(
            broker.save_api_key("nope", "k"),
            Err(ProviderError::UnknownProvider(_))
        ));
        assert!(matches!(
            broker.save_api_key("ollama", "k"),
            Err(ProviderError::UnknownProvider(_))
        ));
    }

    #[test]
    fn forget_removes_both_and_never_errors_on_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let (broker, store) = broker(&tmp);
        assert!(!broker.forget("openai").unwrap());
        broker.save_api_key("openai", "k").unwrap();
        assert!(broker.forget("openai").unwrap());
        assert!(store.is_empty());
        assert!(matches!(
            broker.resolve("openai"),
            Err(ProviderError::NoCredential { .. })
        ));
    }

    #[test]
    fn local_providers_resolve_without_a_credential() {
        let tmp = tempfile::tempdir().unwrap();
        let (broker, _) = broker(&tmp);
        broker.add_local("ollama").unwrap();
        let handle = broker.resolve("ollama").unwrap();
        assert!(!handle.has_value());
        assert_eq!(
            handle
                .authorize("http://127.0.0.1:11434/v1/chat/completions")
                .unwrap(),
            None
        );
        assert!(matches!(
            handle.authorize("http://192.168.1.20:11434/v1/chat/completions"),
            Err(ProviderError::HostNotAllowed { .. })
        ));
        assert!(matches!(
            broker.add_local("openai"),
            Err(ProviderError::UnknownProvider(_))
        ));
    }

    #[test]
    fn plaintext_http_only_on_loopback() {
        let ok = Url::parse("http://localhost:1234/v1").unwrap();
        assert!(validate_scheme(&ok, &[], false).is_ok());
        let bad = Url::parse("http://example.com/v1").unwrap();
        assert!(matches!(
            validate_scheme(&bad, &[], false),
            Err(ProviderError::PlaintextNotLoopback(_))
        ));
        assert!(
            validate_scheme(&bad, &[], true).is_ok(),
            "confirmed dev endpoint"
        );
        let https = Url::parse("https://example.com/v1").unwrap();
        assert!(validate_scheme(&https, &[], false).is_ok());
    }
}
