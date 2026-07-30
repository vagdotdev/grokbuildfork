use anyhow::{Context, Result};

pub trait SecretStore: Send + Sync {
    fn get(&self, provider: &str, field: &str) -> Result<Option<String>>;
    fn set(&self, provider: &str, field: &str, value: &str) -> Result<()>;
    fn delete(&self, provider: &str, field: &str) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct KeyringSecretStore {
    service: String,
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self {
            service: "dev.workshop.auth".to_owned(),
        }
    }
}

impl KeyringSecretStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, provider: &str, field: &str) -> Result<keyring::Entry> {
        let account = format!("{provider}:{field}");
        keyring::Entry::new(&self.service, &account)
            .with_context(|| format!("open operating-system keychain entry for {provider}"))
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, provider: &str, field: &str) -> Result<Option<String>> {
        match self.entry(provider, field)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("read {provider} credentials from operating-system keychain")),
        }
    }

    fn set(&self, provider: &str, field: &str, value: &str) -> Result<()> {
        self.entry(provider, field)?
            .set_password(value)
            .with_context(|| {
                format!("save {provider} credentials in operating-system keychain")
            })
    }

    fn delete(&self, provider: &str, field: &str) -> Result<()> {
        match self.entry(provider, field)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!("delete {provider} credentials from operating-system keychain")
            }),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemorySecretStore {
    values: std::sync::Mutex<std::collections::HashMap<(String, String), String>>,
}

#[cfg(test)]
impl SecretStore for MemorySecretStore {
    fn get(&self, provider: &str, field: &str) -> Result<Option<String>> {
        Ok(self
            .values
            .lock()
            .expect("memory secret store lock")
            .get(&(provider.to_owned(), field.to_owned()))
            .cloned())
    }

    fn set(&self, provider: &str, field: &str, value: &str) -> Result<()> {
        self.values
            .lock()
            .expect("memory secret store lock")
            .insert((provider.to_owned(), field.to_owned()), value.to_owned());
        Ok(())
    }

    fn delete(&self, provider: &str, field: &str) -> Result<()> {
        self.values
            .lock()
            .expect("memory secret store lock")
            .remove(&(provider.to_owned(), field.to_owned()));
        Ok(())
    }
}

