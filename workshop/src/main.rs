use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;
use workshop::oauth::openrouter::OpenRouterOAuth;
use workshop::pi::PiHarness;
use workshop::provider::{OAuthSupport, PROVIDERS, Provider, find_provider};
use workshop::store::{CredentialMethod, CredentialStore};

#[derive(Debug, Parser)]
#[command(name = "workshop", version, about)]
struct Cli {
    #[command(subcommand)]
    command: TopLevel,
}

#[derive(Debug, Subcommand)]
enum TopLevel {
    /// Sign in, sign out, and inspect provider credentials.
    Auth(AuthArgs),
    /// List providers and their supported authentication methods.
    Providers {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Add an authenticated provider model to Grok's model picker.
    Configure(ConfigureArgs),
}

#[derive(Debug, Args)]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Authenticate with an account or API key.
    Login(LoginArgs),
    /// Remove a credential stored by Workshop.
    Logout {
        /// Provider ID. Omit it to choose interactively.
        provider: Option<String>,
    },
    /// Show locally stored credentials without revealing secrets.
    Status {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print a credential for Grok's auth-provider command contract.
    #[command(hide = true)]
    Token(TokenArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum LoginMethod {
    Oauth,
    ApiKey,
}

#[derive(Debug, Args)]
struct LoginArgs {
    /// Provider ID. Omit it to choose interactively.
    provider: Option<String>,
    /// Authentication method. Omit it to choose interactively.
    #[arg(long, value_enum)]
    method: Option<LoginMethod>,
    /// Read an API key from the provider's documented environment variable.
    #[arg(long)]
    from_env: bool,
    /// For OpenRouter, also add this model to Grok after login.
    #[arg(long)]
    model: Option<String>,
    /// Model-picker alias used with --model.
    #[arg(long, requires = "model")]
    alias: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CredentialSource {
    #[default]
    Workshop,
    Pi,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum PiCredentialKind {
    #[default]
    Bearer,
    ApiKey,
}

#[derive(Debug, Args)]
struct TokenArgs {
    provider: String,
    #[arg(long, value_enum, default_value_t)]
    source: CredentialSource,
    /// Required when --source=pi.
    #[arg(long)]
    model: Option<String>,
    #[arg(long, value_enum, default_value_t)]
    pi_credential: PiCredentialKind,
}

#[derive(Debug, Args)]
struct ConfigureArgs {
    /// Currently only OpenRouter can be configured end to end.
    provider: String,
    /// Provider model ID, for example anthropic/claude-sonnet-4.
    model: String,
    /// Optional shorter name in Grok's model picker.
    #[arg(long)]
    alias: Option<String>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        TopLevel::Auth(args) => run_auth(args.command).await,
        TopLevel::Providers { json } => list_providers(json),
        TopLevel::Configure(args) => configure(args).await,
    }
}

async fn run_auth(command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Login(args) => login(args).await,
        AuthCommand::Logout { provider } => {
            let provider = resolve_provider(provider.as_deref())?;
            let store = CredentialStore::discover()?;
            let removed = store.delete(provider.id)?;
            if removed {
                println!("Removed the local {} credential.", provider.name);
            } else {
                println!("No Workshop credential was stored for {}.", provider.name);
            }
            match provider.oauth {
                OAuthSupport::Grok => {
                    run_grok_auth_command("logout")?;
                    println!("Cleared Grok's separate {} session.", provider.name);
                }
                OAuthSupport::Pi => {
                    println!(
                        "Pi credentials are separate. Use Pi's /logout command to remove its {} login.",
                        provider.name
                    );
                }
                OAuthSupport::Direct if provider.id == "openrouter" && removed => {
                    println!(
                        "The OpenRouter API key remains active remotely. Revoke it at https://openrouter.ai/settings/keys if you no longer want it to work."
                    );
                }
                OAuthSupport::Direct | OAuthSupport::None => {}
            }
            Ok(())
        }
        AuthCommand::Status { json } => status(json),
        AuthCommand::Token(args) => print_token(args),
    }
}

async fn login(args: LoginArgs) -> Result<()> {
    let provider = resolve_provider(args.provider.as_deref())?;
    let method = resolve_method(provider, args.method)?;
    match method {
        LoginMethod::ApiKey => login_api_key(provider, args.from_env)?,
        LoginMethod::Oauth => login_oauth(provider).await?,
    }

    if let Some(model) = args.model {
        if provider.id != "openrouter" {
            bail!("automatic Grok model configuration currently supports OpenRouter only");
        }
        configure(ConfigureArgs {
            provider: provider.id.to_owned(),
            model,
            alias: args.alias,
        })
        .await?;
    } else if provider.id == "openrouter" {
        println!("Add a model with: workshop configure openrouter <provider/model-id>");
    }
    Ok(())
}

fn login_api_key(provider: &Provider, from_env: bool) -> Result<()> {
    let store = CredentialStore::discover()?;
    store.preflight(provider.id).with_context(
        || "the operating-system keychain is unavailable; no credential was requested",
    )?;
    let key = if from_env {
        let variable = provider
            .api_key_env
            .context("this provider has no documented API-key environment variable")?;
        std::env::var(variable)
            .with_context(|| format!("{variable} is not set or is not valid UTF-8"))?
    } else {
        if !std::io::stdin().is_terminal() {
            bail!("interactive key entry requires a terminal; use --from-env");
        }
        rpassword::prompt_password(format!("{} API key: ", provider.name))
            .context("read API key")?
    };
    store.save_api_key(provider.id, key.trim())?;
    println!(
        "Saved the {} API key in your operating-system keychain.",
        provider.name
    );
    Ok(())
}

async fn login_oauth(provider: &Provider) -> Result<()> {
    match provider.oauth {
        OAuthSupport::Direct if provider.id == "openrouter" => {
            let store = CredentialStore::discover()?;
            store.preflight(provider.id).with_context(
                || "the operating-system keychain is unavailable; OAuth was not started",
            )?;
            let tokens = OpenRouterOAuth::default().login().await?;
            if let Err(error) = store.save_oauth(
                provider.id,
                &tokens.access,
                tokens.refresh.as_deref(),
                tokens.expires_at_ms,
                tokens.extra,
            ) {
                bail!(
                    "OpenRouter authorized Workshop but the credential could not be stored: {error:#}. Revoke the newly created key at https://openrouter.ai/settings/keys before retrying"
                );
            }
            println!(
                "Signed in to OpenRouter. The minted API key is stored in your operating-system keychain."
            );
            Ok(())
        }
        OAuthSupport::Grok if provider.id == "xai" => run_grok_auth_command("login"),
        OAuthSupport::Pi => {
            let pi = PiHarness::default();
            if !pi.is_available() {
                bail!(
                    "Pi is not installed at {}. Install Pi or set WORKSHOP_PI_BINARY, then authenticate with Pi's /login command",
                    pi.binary().display()
                );
            }
            bail!(
                "{} OAuth is owned by Pi's registered client. Open Pi, run `/login {}`, then configure Workshop to use Pi as the credential source. Workshop deliberately does not copy Pi's OAuth client identity or read its token file",
                provider.name,
                provider.id
            )
        }
        OAuthSupport::None => bail!("{} does not provide OAuth through Workshop", provider.name),
        OAuthSupport::Direct | OAuthSupport::Grok => {
            bail!("{} OAuth is not implemented", provider.name)
        }
    }
}

fn run_grok_auth_command(subcommand: &str) -> Result<()> {
    let binary = std::env::var_os("WORKSHOP_GROK_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("grok"));
    let status = Command::new(&binary)
        .arg(subcommand)
        .status()
        .with_context(|| {
            format!(
                "run Grok {subcommand} at {}; set WORKSHOP_GROK_BINARY if needed",
                binary.display()
            )
        })?;
    if !status.success() {
        bail!("Grok {subcommand} failed with {status}");
    }
    Ok(())
}

fn status(json: bool) -> Result<()> {
    let store = CredentialStore::discover()?;
    let credentials = store.list()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&credentials)?);
        return Ok(());
    }
    println!(
        "Workshop credential metadata: {}",
        store.auth_file_path().display()
    );
    if credentials.is_empty() {
        println!("No credentials stored.");
        return Ok(());
    }
    for (provider, metadata) in credentials {
        let method = match metadata.method {
            CredentialMethod::ApiKey => "API key",
            CredentialMethod::OAuth => "OAuth",
        };
        println!("- {provider}: {method}");
    }
    Ok(())
}

fn print_token(args: TokenArgs) -> Result<()> {
    let output = match args.source {
        CredentialSource::Workshop => {
            let credential = CredentialStore::discover()?
                .get(&args.provider)?
                .with_context(|| {
                    format!(
                        "no Workshop credential for {}; run `workshop auth login {}`",
                        args.provider, args.provider
                    )
                })?;
            workshop::grok::token_json(&credential)?
        }
        CredentialSource::Pi => {
            let model = args
                .model
                .as_deref()
                .context("--model is required with --source=pi")?;
            let pi = PiHarness::default();
            let token = match args.pi_credential {
                PiCredentialKind::Bearer => pi.bearer_token(&args.provider, model)?,
                PiCredentialKind::ApiKey => pi.api_key(&args.provider, model)?,
            };
            workshop::grok::external_token_json(&token, 25 * 60)?
        }
    };
    println!("{output}");
    Ok(())
}

async fn configure(args: ConfigureArgs) -> Result<()> {
    if args.provider != "openrouter" {
        bail!(
            "automatic configuration is only enabled for OpenRouter because its OAuth and inference APIs are documented for third-party clients"
        );
    }
    let store = CredentialStore::discover()?;
    let credential = store
        .get("openrouter")?
        .context("sign in first with `workshop auth login openrouter`")?;
    let context_window =
        OpenRouterOAuth::model_context_window(&credential.access, &args.model).await?;
    let executable = std::env::current_exe().context("locate Workshop executable")?;
    let (path, alias) = workshop::grok::install_openrouter_model(
        &executable,
        &args.model,
        args.alias.as_deref(),
        context_window,
    )?;
    println!("Added `{alias}` to Grok in {}.", path.display());
    println!("Start it with: grok --model {alias}");
    Ok(())
}

fn list_providers(json: bool) -> Result<()> {
    if json {
        let providers: Vec<_> = PROVIDERS
            .iter()
            .map(|provider| {
                serde_json::json!({
                    "id": provider.id,
                    "name": provider.name,
                    "api_key_env": provider.api_key_env,
                    "oauth": match provider.oauth {
                        OAuthSupport::Direct => "workshop",
                        OAuthSupport::Grok => "grok",
                        OAuthSupport::Pi => "pi",
                        OAuthSupport::None => "none",
                    },
                    "grok_compatible": provider.grok_compatible,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&providers)?);
        return Ok(());
    }
    for provider in PROVIDERS {
        let oauth = match provider.oauth {
            OAuthSupport::Direct => "OAuth: Workshop",
            OAuthSupport::Grok => "OAuth: Grok",
            OAuthSupport::Pi => "OAuth: Pi",
            OAuthSupport::None => "OAuth: unavailable",
        };
        let api_key = provider
            .api_key_env
            .map(|name| format!("API key: {name}"))
            .unwrap_or_else(|| "API key: unavailable".to_owned());
        let transport = if provider.grok_compatible {
            "Grok transport: compatible"
        } else {
            "Grok transport: adapter required"
        };
        println!(
            "{:<16} {:<29} {:<34} {}",
            provider.id, oauth, api_key, transport
        );
    }
    Ok(())
}

fn resolve_provider(input: Option<&str>) -> Result<&'static Provider> {
    if let Some(input) = input {
        return find_provider(input)
            .with_context(|| format!("unknown provider {input:?}; run `workshop providers`"));
    }
    if !std::io::stdin().is_terminal() {
        bail!("provider is required when standard input is not a terminal");
    }
    eprintln!("Choose a provider:");
    for (index, provider) in PROVIDERS.iter().enumerate() {
        eprintln!("  {}. {} ({})", index + 1, provider.name, provider.id);
    }
    eprint!("Provider: ");
    use std::io::Write;
    std::io::stderr().flush().context("flush provider prompt")?;
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("read provider selection")?;
    let input = input.trim();
    if let Ok(index) = input.parse::<usize>()
        && let Some(provider) = index.checked_sub(1).and_then(|index| PROVIDERS.get(index))
    {
        return Ok(provider);
    }
    find_provider(input)
        .with_context(|| format!("unknown provider {input:?}; run `workshop providers`"))
}

fn resolve_method(provider: &Provider, method: Option<LoginMethod>) -> Result<LoginMethod> {
    if let Some(method) = method {
        return validate_method(provider, method);
    }
    let oauth_available = provider.oauth != OAuthSupport::None;
    let api_key_available = provider.api_key_env.is_some();
    match (oauth_available, api_key_available) {
        (true, false) => Ok(LoginMethod::Oauth),
        (false, true) => Ok(LoginMethod::ApiKey),
        (false, false) => bail!("{} has no supported authentication method", provider.name),
        (true, true) => {
            if !std::io::stdin().is_terminal() {
                bail!("--method is required when standard input is not a terminal");
            }
            eprintln!("Choose an authentication method:");
            eprintln!("  1. Sign in with an account (OAuth)");
            eprintln!("  2. Enter an API key");
            eprint!("Method: ");
            use std::io::Write;
            std::io::stderr().flush().context("flush method prompt")?;
            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .context("read authentication method")?;
            match input.trim() {
                "1" | "oauth" => Ok(LoginMethod::Oauth),
                "2" | "api-key" | "api_key" => Ok(LoginMethod::ApiKey),
                value => bail!("unknown authentication method {value:?}"),
            }
        }
    }
}

fn validate_method(provider: &Provider, method: LoginMethod) -> Result<LoginMethod> {
    match method {
        LoginMethod::Oauth if provider.oauth == OAuthSupport::None => {
            bail!("{} does not support OAuth", provider.name)
        }
        LoginMethod::ApiKey if provider.api_key_env.is_none() => {
            bail!("{} does not support API-key login", provider.name)
        }
        _ => Ok(method),
    }
}
