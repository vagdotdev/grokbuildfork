//! Secure provider management shared by the CLI and provider TUI.

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

const STORE_VERSION: u32 = 1;
const CONFIG_OWNER: &str = "docking-provider-manager";

#[derive(Debug, clap::Args, Clone)]
pub struct ProviderArgs {
    #[command(subcommand)]
    pub command: ProviderCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum ProviderCommand {
    /// List docked providers without exposing credentials.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Store a provider API key in the OS credential vault.
    Add {
        /// Provider ID: openrouter, openai, anthropic, xai, or a custom name.
        provider: String,
        /// Model ID sent to the provider. When omitted, only the credential is stored.
        #[arg(long)]
        model: Option<String>,
        /// Custom API base URL. Required for unknown providers when --model is used.
        #[arg(long)]
        base_url: Option<String>,
        /// API protocol used by the provider.
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
        /// Header scheme used for the credential.
        #[arg(long, value_enum)]
        auth_scheme: Option<AuthSchemeArg>,
        /// Context window used for compaction decisions.
        #[arg(long, default_value_t = 200_000)]
        context_window: u64,
        /// Make this model the default.
        #[arg(long = "default")]
        make_default: bool,
    },
    /// Remove a provider credential and its owned generated model entry.
    Remove { provider: String },
    /// Import OpenCode credentials into the OS vault.
    ImportOpencode {
        /// OpenCode auth.json path.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Import OAuth subscription records too. They remain adapter-pending.
        #[arg(long)]
        include_oauth: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendArg {
    ChatCompletions,
    Responses,
    Messages,
}

impl BackendArg {
    fn as_config(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSchemeArg {
    Bearer,
    XApiKey,
}

impl AuthSchemeArg {
    fn as_config(self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::XApiKey => "x_api_key",
        }
    }
}

/// A credential payload that is redacted from diagnostics and zeroized on drop.
pub struct ProviderSecret(Zeroizing<String>);

impl ProviderSecret {
    pub fn new(value: String) -> Result<Self> {
        if value.trim().is_empty() {
            bail!("credential cannot be empty");
        }
        Ok(Self(Zeroizing::new(value)))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl std::fmt::Debug for ProviderSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProviderSecret([REDACTED])")
    }
}

/// Typed add request used by both CLI and TUI effects.
pub struct AddProviderRequest {
    pub provider: String,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub backend: Option<BackendArg>,
    pub auth_scheme: Option<AuthSchemeArg>,
    pub context_window: u64,
    pub make_default: bool,
    pub secret: ProviderSecret,
}

impl std::fmt::Debug for AddProviderRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddProviderRequest")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("backend", &self.backend)
            .field("auth_scheme", &self.auth_scheme)
            .field("context_window", &self.context_window)
            .field("make_default", &self.make_default)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ImportOpenCodeRequest {
    pub path: Option<PathBuf>,
    pub include_oauth: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    Ready,
    ConnectedChooseModel,
    VaultedAdapterPending,
    MissingFromVault,
}

impl ProviderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::ConnectedChooseModel => "connected_choose_model",
            Self::VaultedAdapterPending => "vaulted_adapter_pending",
            Self::MissingFromVault => "missing_from_vault",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub auth: String,
    pub source: String,
    pub model: Option<String>,
    pub status: ProviderStatus,
}

#[derive(Debug, Clone)]
pub struct ProviderMutation {
    pub message: String,
    pub providers: Vec<ProviderInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SecretFormat {
    ApiKey,
    OauthJson,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderRecord {
    id: String,
    label: String,
    auth_kind: String,
    secret_format: SecretFormat,
    source: String,
    model_key: Option<String>,
    experimental: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProviderStore {
    version: u32,
    providers: Vec<ProviderRecord>,
    /// Last OpenCode auth.json mtime we auto-imported (unix secs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_opencode_import_mtime: Option<u64>,
}

/// Result of cold-start OpenCode docking (metadata only; never secrets).
#[derive(Debug, Clone)]
pub struct AutoDockReport {
    pub imported: usize,
    pub skipped: usize,
    pub message: String,
}

#[derive(Clone, Copy)]
struct ProviderSpec {
    label: &'static str,
    base_url: &'static str,
    backend: BackendArg,
    auth_scheme: AuthSchemeArg,
}

pub fn run(args: ProviderArgs) -> Result<()> {
    match args.command {
        ProviderCommand::List { json } => print_list(json),
        ProviderCommand::Add {
            provider,
            model,
            base_url,
            backend,
            auth_scheme,
            context_window,
            make_default,
        } => {
            let label = provider_spec(&normalize_provider_id(&provider)?)
                .map(|spec| spec.label)
                .unwrap_or(&provider);
            let secret = rpassword::prompt_password(format!("{label} API key/token: "))
                .context("failed to read credential")?;
            let result = add_provider(AddProviderRequest {
                provider,
                model,
                base_url,
                backend,
                auth_scheme,
                context_window,
                make_default,
                secret: ProviderSecret::new(secret)?,
            })?;
            println!("{}", result.message);
            Ok(())
        }
        ProviderCommand::Remove { provider } => {
            println!("{}", remove_provider(&provider)?.message);
            Ok(())
        }
        ProviderCommand::ImportOpencode {
            path,
            include_oauth,
        } => {
            println!(
                "{}",
                import_opencode(ImportOpenCodeRequest {
                    path,
                    include_oauth,
                })?
                .message
            );
            Ok(())
        }
    }
}

fn print_list(json: bool) -> Result<()> {
    // Same cold-start dock path as the TUI: full-machine credential scan + BYOK config.
    let _ = crate::provider_autodock::auto_dock_on_startup();
    if let Err(error) = bootstrap_byok_auth_from_dock() {
        tracing::warn!(error = %error, "workshop provider bootstrap failed");
        eprintln!("warning: provider bootstrap failed: {error}");
    }
    let rows = list_providers()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("No providers docked.");
        println!("Run `workshop provider add openrouter --model <model-id>`.");
    } else {
        println!("DOCKED PROVIDERS\n");
        for row in rows {
            let marker = if row.status == ProviderStatus::Ready {
                "ready"
            } else {
                "needs attention"
            };
            println!(
                "{:<14} {:<28} {:<16} {}",
                row.id,
                row.status.as_str(),
                row.source,
                marker
            );
        }
    }
    Ok(())
}

/// Read provider metadata and Keychain presence without retrieving secrets.
pub fn list_providers() -> Result<Vec<ProviderInfo>> {
    list_providers_at(&store_path(), &config_path())
}

fn list_providers_at(store_path: &Path, config_path: &Path) -> Result<Vec<ProviderInfo>> {
    let store = read_store_at(store_path)?;
    let config = read_config(config_path)?;
    // Trust metadata for listing — do not hit Keychain here (avoids prompt storms).
    Ok(store
        .providers
        .iter()
        .map(|record| {
            let model = record.model_key.as_deref().and_then(|key| {
                owned_model(&config, key, &record.id)
                    .and_then(|item| item.get("model"))
                    .and_then(toml_edit::Item::as_str)
                    .map(str::to_owned)
            });
            ProviderInfo {
                id: record.id.clone(),
                label: record.label.clone(),
                auth: record.auth_kind.clone(),
                source: record.source.clone(),
                status: status_for(true, record.experimental, model.is_some()),
                model,
            }
        })
        .collect())
}

fn status_for(vaulted: bool, experimental: bool, has_owned_model: bool) -> ProviderStatus {
    if !vaulted {
        ProviderStatus::MissingFromVault
    } else if experimental {
        ProviderStatus::VaultedAdapterPending
    } else if has_owned_model {
        ProviderStatus::Ready
    } else {
        ProviderStatus::ConnectedChooseModel
    }
}

pub fn add_provider(request: AddProviderRequest) -> Result<ProviderMutation> {
    let id = normalize_provider_id(&request.provider)?;
    let known = provider_spec(&id);
    let label = known
        .map(|spec| spec.label)
        .unwrap_or(request.provider.trim())
        .to_owned();
    let mut store = read_store()?;
    let existing_model_key = store
        .providers
        .iter()
        .find(|record| record.id == id)
        .and_then(|record| record.model_key.clone());

    let model_config =
        if let Some(model) = request.model.as_deref().filter(|m| !m.trim().is_empty()) {
            let base_url = request
                .base_url
                .as_deref()
                .or_else(|| known.map(|spec| spec.base_url))
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("--base-url is required for custom providers"))?;
            let backend = request
                .backend
                .or_else(|| known.map(|spec| spec.backend))
                .unwrap_or(BackendArg::ChatCompletions);
            let auth = request
                .auth_scheme
                .or_else(|| known.map(|spec| spec.auth_scheme))
                .unwrap_or(AuthSchemeArg::Bearer);
            Some((model, base_url, backend, auth))
        } else {
            None
        };

    xai_grok_shell::secure_store::set_secret(&id, request.secret.as_bytes())?;
    let model_key = if let Some((model, base_url, backend, auth)) = model_config {
        Some(write_model_config(
            &id,
            &label,
            model,
            base_url,
            backend,
            auth,
            request.context_window,
            request.make_default,
            existing_model_key.as_deref(),
        )?)
    } else {
        existing_model_key
    };
    upsert_record(
        &mut store,
        ProviderRecord {
            id: id.clone(),
            label,
            auth_kind: "api_key".to_owned(),
            secret_format: SecretFormat::ApiKey,
            source: "manual".to_owned(),
            model_key,
            experimental: false,
        },
    );
    write_store(&store)?;
    Ok(ProviderMutation {
        message: if request
            .model
            .as_deref()
            .is_some_and(|m| !m.trim().is_empty())
        {
            format!("{id} connected; model config will reload automatically")
        } else {
            format!("{id} connected; choose a model to make it ready")
        },
        providers: list_providers()?,
    })
}

pub fn remove_provider(provider: &str) -> Result<ProviderMutation> {
    let id = normalize_provider_id(provider)?;
    let mut store = read_store()?;
    let record = store
        .providers
        .iter()
        .find(|record| record.id == id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("provider {id} is not managed by Docking"))?;
    if let Some(model_key) = record.model_key.as_deref() {
        remove_model_config(&record.id, model_key)?;
    }
    xai_grok_shell::secure_store::delete_secret_if_present(&id)?;
    store.providers.retain(|record| record.id != id);
    write_store(&store)?;
    Ok(ProviderMutation {
        message: format!("{id} removed"),
        providers: list_providers()?,
    })
}

pub fn import_opencode(request: ImportOpenCodeRequest) -> Result<ProviderMutation> {
    let path = request.path.unwrap_or_else(default_opencode_path);
    let mtime = file_mtime_secs(&path).ok();
    let (imported, skipped) = import_opencode_from_path(&path, request.include_oauth, mtime)?;
    let oauth_note = if request.include_oauth {
        "; OAuth records are vaulted but adapter-pending"
    } else {
        ""
    };
    Ok(ProviderMutation {
        message: format!("Imported {imported} OpenCode provider(s), skipped {skipped}{oauth_note}"),
        providers: list_providers()?,
    })
}

/// Product bootstrap for Workshop: always pin API-key auth, and when vaulted
/// API keys exist, write usable BYOK model entries so cold start authenticates
/// without Grok OAuth.
pub fn bootstrap_byok_auth_from_dock() -> Result<usize> {
    repair_api_key_metadata()?;
    // One-time: copy any legacy Keychain secrets into the local OpenCode-style vault.
    let store = read_store()?;
    let accounts: Vec<String> = store.providers.iter().map(|p| p.id.clone()).collect();
    let refs: Vec<&str> = accounts.iter().map(String::as_str).collect();
    let migrated = xai_grok_shell::secure_store::migrate_legacy_keychain_accounts(&refs);
    if migrated > 0 {
        tracing::info!(
            migrated,
            "migrated legacy Keychain secrets into local vault"
        );
    }
    let store = read_store()?;
    let mut configured = 0usize;
    let mut first_default: Option<String> = None;

    for record in store.providers.clone() {
        if record.experimental || record.secret_format != SecretFormat::ApiKey {
            continue;
        }
        // Metadata-only: do not probe Keychain during bootstrap (prompt storm).
        let Some(spec) = provider_spec(&record.id) else {
            continue;
        };
        let model_id = default_model_for(&record.id);
        let key = write_model_config(
            &record.id,
            &record.label,
            model_id,
            spec.base_url,
            spec.backend,
            spec.auth_scheme,
            200_000,
            false,
            record.model_key.as_deref(),
        )?;
        // Keep metadata model_key in sync.
        let mut store = read_store()?;
        if let Some(existing) = store.providers.iter_mut().find(|p| p.id == record.id) {
            existing.model_key = Some(key.clone());
        }
        write_store(&store)?;
        if first_default.is_none() {
            first_default = Some(key);
        }
        configured += 1;
    }

    // Discover additional models from each ready provider and write config
    // entries for the top chat models. Errors are non-fatal: the hardcoded
    // defaults above are always sufficient.
    if configured > 0 {
        if let Err(e) = write_discovered_models(&store) {
            tracing::warn!(error = %e, "provider model discovery failed; using defaults");
        }
    }

    // Always pin the product to API-key auth so Grok OAuth is never the default.
    pin_api_key_auth(first_default.as_deref())?;
    Ok(configured)
}

/// Fetch real model lists from docked providers and write additional [model.*]
/// entries for the top chat/completion models. Skips models that already have
/// a config key.
fn write_discovered_models(store: &ProviderStore) -> Result<()> {
    const MAX_MODELS_PER_PROVIDER: usize = 8;

    for record in &store.providers {
        if record.experimental || record.secret_format != SecretFormat::ApiKey {
            continue;
        }
        let Some(spec) = provider_spec(&record.id) else {
            continue;
        };

        let api_key = match xai_grok_shell::secure_store::get_secret(&record.id) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => continue,
        };

        let models = match crate::provider_models::fetch_provider_models(
            &record.id,
            spec.base_url,
            &api_key,
            spec.backend.as_config(),
        ) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(
                    provider = %record.id,
                    error = %e,
                    "skipping model discovery for provider"
                );
                continue;
            }
        };

        let chat_models: Vec<_> = models
            .into_iter()
            .filter(|m| crate::provider_models::is_chat_model(&m.id))
            .take(MAX_MODELS_PER_PROVIDER)
            .collect();

        let path = config_path();
        let mut doc = read_config(&path)?;

        for model in &chat_models {
            // Build a key like "openai/o3" -> "openai-o3" (sanitized).
            let model_key = format!(
                "{}-{}",
                record.id,
                model
                    .id
                    .replace('/', "-")
                    .replace(':', "-")
                    .replace(' ', "-")
            );

            // Skip if this model key already exists.
            if doc.get("model").and_then(|m| m.get(&model_key)).is_some() {
                continue;
            }

            let ctx = model.context_window.unwrap_or(200_000);

            write_owned_model(
                &mut doc,
                &model_key,
                &record.id,
                &spec.label,
                &model.id,
                spec.base_url,
                spec.backend,
                spec.auth_scheme,
                ctx,
            );
        }

        atomic_restrictive_write(&path, doc.to_string().as_bytes())?;
    }
    Ok(())
}

/// If a provider has an owned BYOK model entry but metadata was clobbered to
/// OAuth, restore API-key status without touching Keychain.
fn repair_api_key_metadata() -> Result<()> {
    let config = read_config(&config_path())?;
    let mut store = read_store()?;
    let mut changed = false;
    for record in &mut store.providers {
        if record.secret_format != SecretFormat::OauthJson && !record.experimental {
            continue;
        }
        let key = record.model_key.as_deref().unwrap_or(record.id.as_str());
        if owned_model(&config, key, &record.id).is_some()
            || owned_model(&config, &record.id, &record.id).is_some()
        {
            record.secret_format = SecretFormat::ApiKey;
            record.auth_kind = "api_key".to_owned();
            record.experimental = false;
            if record.model_key.is_none() {
                record.model_key = Some(record.id.clone());
            }
            changed = true;
        }
    }
    if changed {
        write_store(&store)?;
    }
    Ok(())
}

fn default_model_for(provider_id: &str) -> &'static str {
    match provider_id {
        "openai" => "o3",
        "anthropic" => "claude-sonnet-4-20250514",
        "openrouter" => "anthropic/claude-sonnet-4",
        "xai" => "grok-3",
        "groq" => "llama-3.3-70b-versatile",
        "deepseek" => "deepseek-chat",
        "mistral" => "mistral-large-latest",
        "gemini" => "gemini-2.5-flash",
        "together" => "meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8",
        "fireworks" => "accounts/fireworks/models/llama4-maverick-instruct-basic",
        "perplexity" => "sonar-pro",
        _ => "default",
    }
}

fn pin_api_key_auth(default_model_key: Option<&str>) -> Result<()> {
    let path = config_path();
    let mut doc = read_config(&path)?;
    ensure_table(&mut doc, "auth")["preferred_method"] = toml_edit::value("api_key");
    if let Some(key) = default_model_key {
        let has_default = doc
            .get("models")
            .and_then(|m| m.get("default"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty());
        if !has_default {
            ensure_table(&mut doc, "models")["default"] = toml_edit::value(key);
        }
    }
    atomic_restrictive_write(&path, doc.to_string().as_bytes())
}

fn ensure_table<'a>(doc: &'a mut toml_edit::DocumentMut, key: &str) -> &'a mut toml_edit::Table {
    if !doc.get(key).map(toml_edit::Item::is_table).unwrap_or(false) {
        let mut table = toml_edit::Table::new();
        table.set_implicit(false);
        doc[key] = toml_edit::Item::Table(table);
    }
    doc[key].as_table_mut().expect("table just ensured")
}

/// Vault a plaintext API key under `id` (Keychain + metadata only).
/// Skips Keychain rewrite when this provider is already docked.
pub fn vault_api_key(provider: &str, source: &str, secret: &str) -> Result<()> {
    let id = normalize_provider_id(provider)?;
    if secret.trim().is_empty() {
        bail!("credential cannot be empty");
    }
    let mut store = read_store()?;
    let existing = store.providers.iter().find(|record| record.id == id);
    let already_vaulted = xai_grok_shell::secure_store::secret_exists(&id).unwrap_or(false);
    match existing.map(|r| r.secret_format.clone()) {
        Some(SecretFormat::ApiKey) if already_vaulted => {
            // Already docked in the local vault — no rewrite.
            return Ok(());
        }
        Some(SecretFormat::ApiKey) | Some(SecretFormat::OauthJson) | None => {
            // First write, repair missing vault entry, or upgrade OAuth → API key.
            xai_grok_shell::secure_store::set_secret(&id, secret.as_bytes())?;
        }
    }
    let model_key = store
        .providers
        .iter()
        .find(|record| record.id == id)
        .and_then(|record| record.model_key.clone());
    let label = provider_spec(&id)
        .map(|spec| spec.label.to_owned())
        .unwrap_or_else(|| id.clone());
    upsert_record(
        &mut store,
        ProviderRecord {
            id,
            label,
            auth_kind: "api_key".to_owned(),
            secret_format: SecretFormat::ApiKey,
            source: source.to_owned(),
            model_key,
            experimental: false,
        },
    );
    write_store(&store)
}

/// Vault an OAuth/subscription JSON blob (adapter-pending until a real adapter exists).
/// Never overwrites a docked API key.
pub fn vault_oauth_json(provider: &str, source: &str, secret_json: &str) -> Result<()> {
    let id = normalize_provider_id(provider)?;
    if secret_json.trim().is_empty() {
        bail!("credential cannot be empty");
    }
    let mut store = read_store()?;
    if let Some(existing) = store.providers.iter().find(|record| record.id == id) {
        // Never overwrite API keys with OAuth.
        if existing.secret_format == SecretFormat::ApiKey {
            return Ok(());
        }
        // OAuth already docked and vaulted.
        if xai_grok_shell::secure_store::secret_exists(&id).unwrap_or(false) {
            return Ok(());
        }
        // Metadata present but vault empty (migration) — refill once.
        xai_grok_shell::secure_store::set_secret(&id, secret_json.as_bytes())?;
        return Ok(());
    }
    let _ = xai_grok_shell::secure_store::set_secret_if_absent(&id, secret_json.as_bytes())?;
    let model_key = store
        .providers
        .iter()
        .find(|record| record.id == id)
        .and_then(|record| record.model_key.clone());
    let label = provider_spec(&id)
        .map(|spec| spec.label.to_owned())
        .unwrap_or_else(|| id.clone());
    upsert_record(
        &mut store,
        ProviderRecord {
            id,
            label,
            auth_kind: "oauth".to_owned(),
            secret_format: SecretFormat::OauthJson,
            source: source.to_owned(),
            model_key,
            experimental: true,
        },
    );
    write_store(&store)
}

fn import_opencode_from_path(
    path: &Path,
    include_oauth: bool,
    mtime: Option<u64>,
) -> Result<(usize, usize)> {
    let body = Zeroizing::new(
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?,
    );
    let value: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let document = ZeroizingJson(value);
    let providers = document
        .0
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("OpenCode auth file is not a provider object"))?;
    let mut store = read_store()?;
    let mut imported = 0usize;
    let mut skipped = 0usize;

    for (raw_id, value) in providers {
        let Ok(id) = normalize_provider_id(raw_id) else {
            skipped += 1;
            continue;
        };
        let auth_type = value
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let (format, experimental) = match auth_type {
            "api" => {
                let Some(key) = ["key", "apiKey", "token"]
                    .iter()
                    .find_map(|field| value.get(field).and_then(|v| v.as_str()))
                else {
                    skipped += 1;
                    continue;
                };
                xai_grok_shell::secure_store::set_secret(&id, key.as_bytes())?;
                (SecretFormat::ApiKey, false)
            }
            "oauth" if include_oauth => {
                let secret = Zeroizing::new(serde_json::to_string(value)?);
                xai_grok_shell::secure_store::set_secret(&id, secret.as_bytes())?;
                (SecretFormat::OauthJson, true)
            }
            _ => {
                skipped += 1;
                continue;
            }
        };
        let label = provider_spec(&id)
            .map(|spec| spec.label)
            .unwrap_or(raw_id)
            .to_owned();
        let model_key = store
            .providers
            .iter()
            .find(|record| record.id == id)
            .and_then(|record| record.model_key.clone());
        upsert_record(
            &mut store,
            ProviderRecord {
                id,
                label,
                auth_kind: auth_type.to_owned(),
                secret_format: format,
                source: "opencode".to_owned(),
                model_key,
                experimental,
            },
        );
        imported += 1;
    }
    if let Some(mtime) = mtime {
        store.last_opencode_import_mtime = Some(mtime);
    }
    write_store(&store)?;
    Ok((imported, skipped))
}

fn file_mtime_secs(path: &Path) -> Result<u64> {
    let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    Ok(meta
        .modified()
        .with_context(|| format!("mtime {}", path.display()))?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs())
}

fn default_opencode_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/opencode/auth.json")
}

struct ZeroizingJson(serde_json::Value);

impl Drop for ZeroizingJson {
    fn drop(&mut self) {
        zeroize_json(&mut self.0);
    }
}

fn zeroize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(value) => value.zeroize(),
        serde_json::Value::Array(values) => values.iter_mut().for_each(zeroize_json),
        serde_json::Value::Object(values) => values.values_mut().for_each(zeroize_json),
        _ => {}
    }
}

fn provider_spec(id: &str) -> Option<ProviderSpec> {
    match id {
        "openrouter" => Some(ProviderSpec {
            label: "OpenRouter",
            base_url: "https://openrouter.ai/api/v1",
            backend: BackendArg::ChatCompletions,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        "openai" => Some(ProviderSpec {
            label: "OpenAI",
            base_url: "https://api.openai.com/v1",
            backend: BackendArg::Responses,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        "anthropic" => Some(ProviderSpec {
            label: "Anthropic",
            base_url: "https://api.anthropic.com/v1",
            backend: BackendArg::Messages,
            auth_scheme: AuthSchemeArg::XApiKey,
        }),
        "xai" => Some(ProviderSpec {
            label: "xAI",
            base_url: "https://api.x.ai/v1",
            backend: BackendArg::Responses,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        "groq" => Some(ProviderSpec {
            label: "Groq",
            base_url: "https://api.groq.com/openai/v1",
            backend: BackendArg::ChatCompletions,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        "deepseek" => Some(ProviderSpec {
            label: "DeepSeek",
            base_url: "https://api.deepseek.com/v1",
            backend: BackendArg::ChatCompletions,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        "mistral" => Some(ProviderSpec {
            label: "Mistral",
            base_url: "https://api.mistral.ai/v1",
            backend: BackendArg::ChatCompletions,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        "gemini" => Some(ProviderSpec {
            label: "Gemini",
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            backend: BackendArg::ChatCompletions,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        "together" => Some(ProviderSpec {
            label: "Together",
            base_url: "https://api.together.xyz/v1",
            backend: BackendArg::ChatCompletions,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        "fireworks" => Some(ProviderSpec {
            label: "Fireworks",
            base_url: "https://api.fireworks.ai/inference/v1",
            backend: BackendArg::ChatCompletions,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        "perplexity" => Some(ProviderSpec {
            label: "Perplexity",
            base_url: "https://api.perplexity.ai",
            backend: BackendArg::ChatCompletions,
            auth_scheme: AuthSchemeArg::Bearer,
        }),
        _ => None,
    }
}

fn normalize_provider_id(value: &str) -> Result<String> {
    let id = value.trim().to_ascii_lowercase();
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("provider ID may contain only letters, numbers, dash, underscore, and dot");
    }
    Ok(id)
}

fn upsert_record(store: &mut ProviderStore, record: ProviderRecord) {
    if let Some(existing) = store.providers.iter_mut().find(|item| item.id == record.id) {
        *existing = record;
    } else {
        store.providers.push(record);
        store.providers.sort_by(|a, b| a.id.cmp(&b.id));
    }
}

fn store_path() -> PathBuf {
    xai_grok_config::grok_home().join("providers.json")
}

fn config_path() -> PathBuf {
    xai_grok_config::grok_home().join("config.toml")
}

fn read_store() -> Result<ProviderStore> {
    read_store_at(&store_path())
}

fn read_store_at(path: &Path) -> Result<ProviderStore> {
    match fs::read_to_string(path) {
        Ok(body) => serde_json::from_str(&body)
            .with_context(|| format!("failed to parse {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ProviderStore {
            version: STORE_VERSION,
            providers: Vec::new(),
            last_opencode_import_mtime: None,
        }),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn write_store(store: &ProviderStore) -> Result<()> {
    let mut body = serde_json::to_vec_pretty(store)?;
    body.push(b'\n');
    atomic_restrictive_write(&store_path(), &body)
}

fn atomic_restrictive_write(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid provider configuration path"))?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("providers"),
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(body)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn read_config(path: &Path) -> Result<toml_edit::DocumentMut> {
    crate::config_toml_edit::read_config_document_for_edit(path).ok_or_else(|| {
        anyhow::anyhow!(
            "{} is invalid TOML; refusing to overwrite it",
            path.display()
        )
    })
}

fn owned_model<'a>(
    doc: &'a toml_edit::DocumentMut,
    key: &str,
    provider_id: &str,
) -> Option<&'a dyn toml_edit::TableLike> {
    let item = doc.get("model")?.get(key)?;
    let table = item.as_table_like()?;
    (table
        .get("provider_manager_owner")
        .and_then(toml_edit::Item::as_str)
        == Some(CONFIG_OWNER)
        && table
            .get("provider_manager_provider_id")
            .and_then(toml_edit::Item::as_str)
            == Some(provider_id))
    .then_some(table)
}

fn choose_model_key(
    doc: &toml_edit::DocumentMut,
    provider_id: &str,
    recorded_key: Option<&str>,
) -> String {
    if let Some(key) = recorded_key
        && owned_model(doc, key, provider_id).is_some()
    {
        return key.to_owned();
    }
    let base = provider_id.to_owned();
    if owned_model(doc, &base, provider_id).is_some() {
        return base;
    }
    if doc
        .get("model")
        .and_then(|models| models.get(&base))
        .is_none()
    {
        return base;
    }
    for suffix in 1u64.. {
        let candidate = if suffix == 1 {
            format!("{provider_id}-docking")
        } else {
            format!("{provider_id}-docking-{suffix}")
        };
        if owned_model(doc, &candidate, provider_id).is_some() {
            return candidate;
        }
        if doc
            .get("model")
            .and_then(|models| models.get(&candidate))
            .is_none()
        {
            return candidate;
        }
    }
    unreachable!()
}

#[allow(clippy::too_many_arguments)]
fn write_model_config(
    provider_id: &str,
    label: &str,
    model: &str,
    base_url: &str,
    backend: BackendArg,
    auth_scheme: AuthSchemeArg,
    context_window: u64,
    make_default: bool,
    recorded_key: Option<&str>,
) -> Result<String> {
    if model.trim().is_empty() || context_window == 0 {
        bail!("model must be non-empty and context-window must be greater than zero");
    }
    let path = config_path();
    let mut doc = read_config(&path)?;
    let key = choose_model_key(&doc, provider_id, recorded_key);
    write_owned_model(
        &mut doc,
        &key,
        provider_id,
        label,
        model,
        base_url,
        backend,
        auth_scheme,
        context_window,
    );
    if make_default {
        doc["models"]["default"] = toml_edit::value(&key);
    }
    atomic_restrictive_write(&path, doc.to_string().as_bytes())?;
    Ok(key)
}

#[allow(clippy::too_many_arguments)]
fn write_owned_model(
    doc: &mut toml_edit::DocumentMut,
    key: &str,
    provider_id: &str,
    label: &str,
    model: &str,
    base_url: &str,
    backend: BackendArg,
    auth_scheme: AuthSchemeArg,
    context_window: u64,
) {
    let models = ensure_table(doc, "model");
    if !models
        .get(key)
        .map(toml_edit::Item::is_table)
        .unwrap_or(false)
    {
        let mut table = toml_edit::Table::new();
        table.set_implicit(false);
        models[key] = toml_edit::Item::Table(table);
    }
    let entry = models[key]
        .as_table_mut()
        .expect("model table just ensured");
    entry["model"] = toml_edit::value(model);
    entry["base_url"] = toml_edit::value(base_url);
    entry["name"] = toml_edit::value(format!("{label} / {model}"));
    entry["env_key"] = toml_edit::value(xai_grok_shell::secure_store::reference(provider_id));
    entry["api_backend"] = toml_edit::value(backend.as_config());
    entry["auth_scheme"] = toml_edit::value(auth_scheme.as_config());
    entry["context_window"] = toml_edit::value(context_window as i64);
    entry["provider_manager_owner"] = toml_edit::value(CONFIG_OWNER);
    entry["provider_manager_provider_id"] = toml_edit::value(provider_id);
}

fn remove_model_config(provider_id: &str, model_key: &str) -> Result<()> {
    let path = config_path();
    if !path.exists() {
        return Ok(());
    }
    let mut doc = read_config(&path)?;
    let removed = remove_owned_model(&mut doc, provider_id, model_key);
    if removed {
        atomic_restrictive_write(&path, doc.to_string().as_bytes())?;
    }
    Ok(())
}

fn remove_owned_model(
    doc: &mut toml_edit::DocumentMut,
    provider_id: &str,
    model_key: &str,
) -> bool {
    if owned_model(doc, model_key, provider_id).is_none() {
        return false;
    }
    if let Some(models) = doc
        .get_mut("model")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        models.remove(model_key);
    }
    if doc["models"]["default"].as_str() == Some(model_key) {
        doc["models"]
            .as_table_like_mut()
            .map(|table| table.remove("default"));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_prioritizes_missing_and_oauth() {
        assert_eq!(
            status_for(false, true, true),
            ProviderStatus::MissingFromVault
        );
        assert_eq!(
            status_for(true, true, true),
            ProviderStatus::VaultedAdapterPending
        );
        assert_eq!(
            status_for(true, false, false),
            ProviderStatus::ConnectedChooseModel
        );
        assert_eq!(status_for(true, false, true), ProviderStatus::Ready);
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = ProviderSecret::new("sk-plain-text".into()).unwrap();
        let debug = format!("{secret:?}");
        assert_eq!(debug, "ProviderSecret([REDACTED])");
        assert!(!debug.contains("sk-plain-text"));
    }

    #[test]
    fn add_request_debug_is_redacted() {
        let request = AddProviderRequest {
            provider: "openai".into(),
            model: None,
            base_url: None,
            backend: None,
            auth_scheme: None,
            context_window: 200_000,
            make_default: false,
            secret: ProviderSecret::new("sk-plain-text".into()).unwrap(),
        };
        let debug = format!("{request:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("sk-plain-text"));
    }

    #[test]
    fn collision_uses_safe_generated_key() {
        let mut doc: toml_edit::DocumentMut =
            "[model.openai]\nmodel = \"manual\"\n".parse().unwrap();
        assert_eq!(choose_model_key(&doc, "openai", None), "openai-docking");
        write_owned_model(
            &mut doc,
            "openai-docking",
            "openai",
            "OpenAI",
            "gpt-5",
            "https://api.openai.com/v1",
            BackendArg::Responses,
            AuthSchemeArg::Bearer,
            200_000,
        );
        assert_eq!(
            choose_model_key(&doc, "openai", Some("openai-docking")),
            "openai-docking"
        );
        assert_eq!(choose_model_key(&doc, "openai", None), "openai-docking");
    }

    #[test]
    fn removal_requires_matching_ownership() {
        let mut doc: toml_edit::DocumentMut = "[model.manual]\nmodel = \"keep\"\n\n[model.owned]\nmodel = \"drop\"\nprovider_manager_owner = \"docking-provider-manager\"\nprovider_manager_provider_id = \"openai\"\n\n[models]\ndefault = \"owned\"\n".parse().unwrap();
        assert!(!remove_owned_model(&mut doc, "openai", "manual"));
        assert!(doc["model"]["manual"].is_table_like());
        assert!(!remove_owned_model(&mut doc, "anthropic", "owned"));
        assert!(remove_owned_model(&mut doc, "openai", "owned"));
        assert!(doc.get("model").and_then(|m| m.get("owned")).is_none());
        assert_eq!(doc["models"]["default"].as_str(), None);
    }

    #[test]
    fn metadata_roundtrip_contains_no_secret() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("providers.json");
        let store = ProviderStore {
            version: STORE_VERSION,
            providers: vec![ProviderRecord {
                id: "openrouter".into(),
                label: "OpenRouter".into(),
                auth_kind: "api_key".into(),
                secret_format: SecretFormat::ApiKey,
                source: "manual".into(),
                model_key: Some("openrouter".into()),
                experimental: false,
            }],
            last_opencode_import_mtime: None,
        };
        let body = serde_json::to_vec_pretty(&store).unwrap();
        atomic_restrictive_write(&path, &body).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(!body.contains("sk-"));
        assert_eq!(read_store_at(&path).unwrap().providers.len(), 1);
    }

    #[test]
    fn rejects_unsafe_provider_ids() {
        assert!(normalize_provider_id("open-router_1").is_ok());
        assert!(normalize_provider_id("../openai").is_err());
    }
}
