//! Workshop overlay: connection picker data loading and selection activation.
//!
//! Models rows come from `workshop-providers`, rail state from `workshop-detect`, and the OpenCode
//! free catalog from the engine cache. Activating a Direct API / Local row writes
//! `[model.<key>]` + `default` into `$WORKSHOP_HOME/config.toml`, exports a saved key as its
//! `WORKSHOP_<PROVIDER>_API_KEY` env var (never into the file), asks the shell to reload its model
//! list, and authenticates with the non-interactive method. Nothing here starts an OAuth flow.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use workshop_auth::{EngineModel, PickerSnapshot, models_rows};
use workshop_providers::{
    Catalog, CredentialBroker, CredentialInjection, FileSecretStore, KeyringSecretStore,
    LayeredSecretStore, resolve_model_entry, select_default,
};

/// Which runtime a prompt is routed through.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkshopConnection {
    /// The shell's own agent loop (Direct API / Local `[model.<key>]`), the default.
    Shell,
    /// A free model behind the OpenCode engine (`opencode serve`).
    Engine { model: EngineModel },
    /// A vendor CLI adapter on a Ready subscription rail.
    Adapter {
        rail: workshop_detect::Rail,
        model: workshop_detect::ModelRef,
    },
}

impl WorkshopConnection {
    /// Composer label: `Claude · {model}` for rails, `OpenCode · {model}` for the engine.
    pub fn composer_label(&self) -> Option<String> {
        match self {
            Self::Shell => None,
            Self::Engine { model } => Some(format!("OpenCode · {}", model.name)),
            Self::Adapter { rail, model } => Some(workshop_detect::composer_label(*rail, model)),
        }
    }
    pub fn is_shell(&self) -> bool {
        matches!(self, Self::Shell)
    }
}

/// `$WORKSHOP_HOME` (created if missing).
pub fn workshop_home() -> PathBuf {
    xai_dirs::grok_home()
}

/// The credential broker over the OS keyring with the owner-only file fallback under the home.
pub fn default_broker() -> CredentialBroker {
    let home = workshop_home();
    let tag = home
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "workshop".into());
    let store = LayeredSecretStore::new(
        Box::new(KeyringSecretStore::for_home(&tag)),
        FileSecretStore::new(home.join("secrets")),
    );
    CredentialBroker::new(Arc::new(store), home.join("connections.json"))
}

fn engine_cache_path() -> PathBuf {
    workshop_home()
        .join("catalog-cache")
        .join("opencode-engine.json")
}

/// The last engine catalog this home saw (written after every engine launch).
pub fn cached_engine_models() -> Vec<EngineModel> {
    std::fs::read_to_string(engine_cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn store_engine_models(models: &[EngineModel]) {
    let path = engine_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(models) {
        let _ = workshop_providers::atomic_write_private(&path, &json);
    }
}

/// Build the picker snapshot: loopback local-server probe, builtin + cached catalogs, broker
/// connection state, engine cache, and the CLI rail probe (child processes, so on the blocking pool).
pub async fn load_picker_snapshot() -> PickerSnapshot {
    let local = workshop_providers::probe_all_local_servers(Duration::from_millis(600)).await;
    let mut catalog = Catalog::builtin();
    let as_of = "live";
    for status in &local {
        if status.is_reachable()
            && let Some(m) = workshop_providers::manifest(&status.provider_id)
        {
            catalog.replace_provider(
                &status.provider_id,
                workshop_providers::local::local_rows(&m, status, as_of),
            );
        }
    }
    let broker = default_broker();
    let secret_backend = broker.secret_backend();
    let engine = cached_engine_models();
    let rows = models_rows(&catalog, |id| broker.is_connected(id), &engine);
    let default_selection = Some(select_default(&local, true));
    let rails = tokio::task::spawn_blocking(|| {
        let probe = workshop_detect::probe_all(&workshop_detect::DetectConfig::default());
        workshop_detect::rails(&probe, workshop_detect::model::default_models).to_vec()
    })
    .await
    .unwrap_or_else(|_| {
        workshop_detect::Rail::ALL
            .iter()
            .map(|r| workshop_detect::RailState::detecting(*r))
            .collect()
    });
    PickerSnapshot {
        rows,
        rails,
        default_selection,
        secret_backend: Some(secret_backend),
    }
}

/// What activating a Direct API / Local row needs the process to do.
#[derive(Debug, Clone)]
pub struct ActivationPlan {
    /// `[model.<key>]` key written to config.toml (and set as `default`).
    pub key: String,
    pub display_name: String,
    pub base_url: String,
    /// Env var exports the shell needs before it reloads (saved-key providers only).
    pub env: Vec<(String, String)>,
}

/// Resolve `model` against the broker, write it to config.toml, and describe the follow-up.
pub fn activate_catalog_model(
    model: &workshop_providers::CatalogModel,
) -> Result<ActivationPlan, String> {
    let broker = default_broker();
    let spec = resolve_model_entry(model, &broker).map_err(|e| e.to_string())?;
    let mut env = Vec::new();
    if let CredentialInjection::FromBroker { provider_id, var } = &spec.credential {
        let handle = broker.resolve(provider_id).map_err(|e| e.to_string())?;
        let value = handle
            .authorize(&model.endpoint())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no saved credential for {provider_id}"))?
            .to_owned();
        env.push((var.clone(), value));
    }
    let key = workshop_auth::config_write::activate_model(&workshop_auth::config_path(), &spec)
        .map_err(|e| e.to_string())?;
    Ok(ActivationPlan {
        key,
        display_name: spec.name.clone(),
        base_url: spec.base_url.clone(),
        env,
    })
}

/// Export the saved-key env vars in this process (the agent runs in-process and reads them on reload).
pub fn export_env(env: &[(String, String)]) {
    for (k, v) in env {
        // SAFETY: single-threaded UI thread mutating process env before the agent's reload request
        // is sent; readers (`std::env::var`) tolerate concurrent reads on Unix targets we ship.
        unsafe { std::env::set_var(k, v) };
    }
}

/// Persist a pasted key for `provider_id` in the broker (keyring, else the 0600 file fallback).
pub fn save_provider_key(provider_id: &str, key: &str) -> Result<&'static str, String> {
    let broker = default_broker();
    broker
        .save_api_key(provider_id, key)
        .map_err(|e| e.to_string())?;
    Ok(broker.secret_backend())
}

/// Key-entry prompt label for a provider's paste-key connect flow.
pub fn key_prompt_label(provider_id: &str) -> String {
    match workshop_providers::manifest(provider_id) {
        Some(m) => match &m.credential_url {
            Some(url) => format!("Paste your {} API key (get one at {url})", m.display_name),
            None => format!("Paste your {} API key", m.display_name),
        },
        None => format!("Paste your {provider_id} API key"),
    }
}

/// OpenRouter PKCE sign-in: opens the browser, waits for the loopback callback, exchanges the code,
/// saves the key. Returns the secret backend used.
pub async fn openrouter_sign_in() -> Result<&'static str, String> {
    use workshop_providers::{OpenRouterSignIn, SignInMode};
    let signin = OpenRouterSignIn::start(SignInMode::Loopback)
        .await
        .map_err(|e| e.to_string())?;
    let url = signin.authorize_url();
    if webbrowser::open(&url).is_err() {
        tracing::warn!("could not open a browser for OpenRouter sign-in; url: {url}");
    }
    let client = workshop_providers::oauth::openrouter::exchange_client().map_err(|e| e.to_string())?;
    let key = signin
        .complete_loopback(&client, Duration::from_secs(300))
        .await
        .map_err(|e| e.to_string())?;
    save_provider_key("openrouter", &key)
}

/// The official CLI login command for a rail, to run attached to the user's terminal.
pub fn rail_login_argv(rail: workshop_detect::Rail) -> Vec<String> {
    let vendor = rail.vendor();
    let cfg = workshop_detect::DetectConfig::default();
    let probe = workshop_detect::probe_vendor(vendor, &cfg);
    let bin = probe
        .binary
        .as_ref()
        .map(|b| b.path.to_string_lossy().to_string())
        .unwrap_or_else(|| vendor.binary_names().first().copied().unwrap_or("").to_owned());
    let mut argv = vec![bin];
    argv.extend(
        workshop_detect::login_argv(vendor)
            .iter()
            .map(|s| s.to_string()),
    );
    argv
}
