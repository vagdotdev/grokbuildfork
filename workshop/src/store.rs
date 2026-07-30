use crate::secrets::{KeyringSecretStore, SecretStore};
use anyhow::{Context, Result, anyhow, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const AUTH_FILE_VERSION: u32 = 1;
const CREDENTIAL_FIELD: &str = "credential";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMethod {
    ApiKey,
    OAuth,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CredentialMetadata {
    pub method: CredentialMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct AuthFile {
    version: u32,
    #[serde(default)]
    providers: BTreeMap<String, CredentialMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Credential {
    pub metadata: CredentialMetadata,
    pub access: String,
    pub refresh: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct StoredSecretCredential {
    metadata: CredentialMetadata,
    access: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh: Option<String>,
}

pub struct CredentialStore<S = KeyringSecretStore> {
    home: PathBuf,
    secrets: S,
}

impl CredentialStore<KeyringSecretStore> {
    pub fn discover() -> Result<Self> {
        let home = default_workshop_home()?;
        let secrets = KeyringSecretStore::for_home(&home);
        Ok(Self::new(home, secrets))
    }
}

impl<S: SecretStore> CredentialStore<S> {
    pub fn new(home: PathBuf, secrets: S) -> Self {
        Self { home, secrets }
    }

    pub fn auth_file_path(&self) -> PathBuf {
        self.home.join("auth.json")
    }

    pub fn list(&self) -> Result<BTreeMap<String, CredentialMetadata>> {
        self.with_read_lock(|| Ok(self.read_auth_file()?.providers))
    }

    pub fn get(&self, provider: &str) -> Result<Option<Credential>> {
        validate_provider_id(provider)?;
        self.with_read_lock(|| {
            let Some(secret) = self.secrets.get(provider, CREDENTIAL_FIELD)? else {
                return Ok(None);
            };
            let secret: StoredSecretCredential = serde_json::from_str(&secret)
                .with_context(|| format!("parse {provider} credential from keychain"))?;
            Ok(Some(Credential {
                metadata: secret.metadata,
                access: secret.access,
                refresh: secret.refresh,
            }))
        })
    }

    pub fn preflight(&self, provider: &str) -> Result<()> {
        validate_provider_id(provider)?;
        self.with_write_lock(|| {
            let field = format!("preflight-{}", uuid::Uuid::new_v4());
            let value = uuid::Uuid::new_v4().to_string();
            self.secrets.set(provider, &field, &value)?;
            let read_result = self.secrets.get(provider, &field);
            let delete_result = self.secrets.delete(provider, &field);
            let read = read_result?;
            delete_result?;
            if read.as_deref() != Some(value.as_str()) {
                bail!("operating-system keychain failed its write/read check");
            }
            Ok(())
        })
    }

    pub fn save_api_key(&self, provider: &str, key: &str) -> Result<()> {
        if key.trim().is_empty() {
            bail!("API key cannot be empty");
        }
        self.save(
            provider,
            CredentialMetadata {
                method: CredentialMethod::ApiKey,
                expires_at_ms: None,
                extra: BTreeMap::new(),
            },
            key,
            None,
        )
    }

    pub fn save_oauth(
        &self,
        provider: &str,
        access: &str,
        refresh: Option<&str>,
        expires_at_ms: Option<u64>,
        extra: BTreeMap<String, String>,
    ) -> Result<()> {
        if access.trim().is_empty() {
            bail!("OAuth access token cannot be empty");
        }
        self.save(
            provider,
            CredentialMetadata {
                method: CredentialMethod::OAuth,
                expires_at_ms,
                extra,
            },
            access,
            refresh,
        )
    }

    fn save(
        &self,
        provider: &str,
        metadata: CredentialMetadata,
        access: &str,
        refresh: Option<&str>,
    ) -> Result<()> {
        validate_provider_id(provider)?;
        self.with_write_lock(|| {
            let mut auth = self.read_auth_file()?;
            let old_credential = self.secrets.get(provider, CREDENTIAL_FIELD)?;
            let secret = serde_json::to_string(&StoredSecretCredential {
                metadata: metadata.clone(),
                access: access.to_owned(),
                refresh: refresh
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
            })
            .context("serialize credential for keychain")?;

            if let Err(error) = self.secrets.set(provider, CREDENTIAL_FIELD, &secret) {
                return rollback_error(
                    error,
                    self.restore_secret(provider, CREDENTIAL_FIELD, old_credential.as_deref()),
                );
            }

            auth.providers.insert(provider.to_owned(), metadata);
            if let Err(error) = self.write_auth_file(&auth) {
                let rollback =
                    self.restore_secret(provider, CREDENTIAL_FIELD, old_credential.as_deref());
                return rollback_error(error, rollback);
            }
            Ok(())
        })
    }

    pub fn delete(&self, provider: &str) -> Result<bool> {
        validate_provider_id(provider)?;
        self.with_write_lock(|| {
            let mut auth = self.read_auth_file()?;
            let old_credential = self.secrets.get(provider, CREDENTIAL_FIELD)?;
            let existed = auth.providers.contains_key(provider) || old_credential.is_some();

            if let Err(error) = self.secrets.delete(provider, CREDENTIAL_FIELD) {
                return rollback_error(
                    error,
                    self.restore_secret(provider, CREDENTIAL_FIELD, old_credential.as_deref()),
                );
            }
            auth.providers.remove(provider);
            if let Err(error) = self.write_auth_file(&auth) {
                let rollback =
                    self.restore_secret(provider, CREDENTIAL_FIELD, old_credential.as_deref());
                return rollback_error(error, rollback);
            }
            Ok(existed)
        })
    }

    fn restore_secret(&self, provider: &str, field: &str, value: Option<&str>) -> Result<()> {
        if let Some(value) = value {
            self.secrets.set(provider, field, value)
        } else {
            self.secrets.delete(provider, field)
        }
    }

    fn read_auth_file(&self) -> Result<AuthFile> {
        let path = self.auth_file_path();
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AuthFile {
                    version: AUTH_FILE_VERSION,
                    providers: BTreeMap::new(),
                });
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let auth: AuthFile =
            serde_json::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;
        if auth.version != AUTH_FILE_VERSION {
            bail!(
                "unsupported Workshop auth file version {} in {}",
                auth.version,
                path.display()
            );
        }
        Ok(auth)
    }

    fn write_auth_file(&self, auth: &AuthFile) -> Result<()> {
        ensure_private_dir(&self.home)?;
        let path = self.auth_file_path();
        let temp = self
            .home
            .join(format!(".auth.json.{}.tmp", uuid::Uuid::new_v4()));
        let mut contents = serde_json::to_vec_pretty(auth).context("serialize auth metadata")?;
        contents.push(b'\n');

        let mut file = private_file(&temp)?;
        file.write_all(&contents)
            .with_context(|| format!("write {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temp.display()))?;
        drop(file);
        if let Err(error) = std::fs::rename(&temp, &path) {
            let _ = std::fs::remove_file(&temp);
            return Err(error).with_context(|| format!("replace {}", path.display()));
        }
        Ok(())
    }

    fn with_read_lock<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        self.with_lock(false, operation)
    }

    fn with_write_lock<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        self.with_lock(true, operation)
    }

    fn with_lock<T>(&self, exclusive: bool, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        ensure_private_dir(&self.home)?;
        let lock_path = self.home.join("auth.lock");
        let lock = private_file(&lock_path)?;
        if exclusive {
            FileExt::lock_exclusive(&lock)
        } else {
            FileExt::lock_shared(&lock)
        }
        .with_context(|| format!("lock {}", lock_path.display()))?;
        let result = operation();
        let unlock =
            FileExt::unlock(&lock).with_context(|| format!("unlock {}", lock_path.display()));
        result.and_then(|value| unlock.map(|()| value))
    }
}

fn rollback_error<T>(error: anyhow::Error, rollback: Result<()>) -> Result<T> {
    match rollback {
        Ok(()) => Err(error),
        Err(rollback) => Err(anyhow!(
            "{error:#}; restoring the prior keychain credential also failed: {rollback:#}"
        )),
    }
}

pub fn default_workshop_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("WORKSHOP_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let base = directories::BaseDirs::new().context("determine home directory")?;
    Ok(base.home_dir().join(".workshop"))
}

fn validate_provider_id(provider: &str) -> Result<()> {
    if provider.is_empty()
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("invalid provider id {provider:?}");
    }
    Ok(())
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("set private permissions on {}", path.display()))?;
    }
    Ok(())
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("set private permissions on {}", path.display()))?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecretStore;

    #[derive(Clone, Default)]
    struct FailingSecretStore {
        values:
            std::sync::Arc<std::sync::Mutex<std::collections::HashMap<(String, String), String>>>,
        fail_credential_write_once: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl SecretStore for FailingSecretStore {
        fn get(&self, provider: &str, field: &str) -> Result<Option<String>> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(&(provider.to_owned(), field.to_owned()))
                .cloned())
        }

        fn set(&self, provider: &str, field: &str, value: &str) -> Result<()> {
            self.values
                .lock()
                .unwrap()
                .insert((provider.to_owned(), field.to_owned()), value.to_owned());
            if field == CREDENTIAL_FIELD
                && self
                    .fail_credential_write_once
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                bail!("injected credential write failure");
            }
            Ok(())
        }

        fn delete(&self, provider: &str, field: &str) -> Result<()> {
            self.values
                .lock()
                .unwrap()
                .remove(&(provider.to_owned(), field.to_owned()));
            Ok(())
        }
    }

    #[test]
    fn secrets_stay_out_of_metadata_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(temp.path().to_owned(), MemorySecretStore::default());
        store
            .save_oauth(
                "openrouter",
                "secret-access",
                Some("secret-refresh"),
                Some(123),
                BTreeMap::new(),
            )
            .unwrap();

        let contents = std::fs::read_to_string(store.auth_file_path()).unwrap();
        assert!(!contents.contains("secret-access"));
        assert!(!contents.contains("secret-refresh"));
        let credential = store.get("openrouter").unwrap().unwrap();
        assert_eq!(credential.access, "secret-access");
        assert_eq!(credential.refresh.as_deref(), Some("secret-refresh"));
    }

    #[test]
    fn replacing_oauth_with_api_key_removes_refresh_token() {
        let temp = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(temp.path().to_owned(), MemorySecretStore::default());
        store
            .save_oauth(
                "provider",
                "access",
                Some("refresh"),
                Some(123),
                BTreeMap::new(),
            )
            .unwrap();
        store.save_api_key("provider", "key").unwrap();

        let credential = store.get("provider").unwrap().unwrap();
        assert_eq!(credential.metadata.method, CredentialMethod::ApiKey);
        assert_eq!(credential.access, "key");
        assert_eq!(credential.refresh, None);
    }

    #[test]
    fn failed_keyring_update_restores_previous_credential() {
        let temp = tempfile::tempdir().unwrap();
        let secrets = FailingSecretStore::default();
        let control = secrets.clone();
        let store = CredentialStore::new(temp.path().to_owned(), secrets);
        store.save_api_key("provider", "old-key").unwrap();
        control
            .fail_credential_write_once
            .store(true, std::sync::atomic::Ordering::SeqCst);

        assert!(
            store
                .save_oauth(
                    "provider",
                    "new-access",
                    Some("new-refresh"),
                    Some(123),
                    BTreeMap::new(),
                )
                .is_err()
        );
        let credential = store.get("provider").unwrap().unwrap();
        assert_eq!(credential.metadata.method, CredentialMethod::ApiKey);
        assert_eq!(credential.access, "old-key");
        assert_eq!(credential.refresh, None);
    }

    #[test]
    fn rejects_unsafe_provider_ids() {
        let temp = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(temp.path().to_owned(), MemorySecretStore::default());
        assert!(store.save_api_key("../escape", "key").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn metadata_permissions_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(temp.path().to_owned(), MemorySecretStore::default());
        store.save_api_key("provider", "key").unwrap();
        let mode = std::fs::metadata(store.auth_file_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
