//! The signed-in account's real model list, asked of each vendor's official CLI.
//!
//! | Rail   | Command                                                    | Read                                                   |
//! |--------|------------------------------------------------------------|--------------------------------------------------------|
//! | Claude | `claude -p --input-format stream-json …` + `initialize`    | `models[]` (`value`, `displayName`), `account`         |
//! | Codex  | `codex app-server`: `account/read`, `model/list`           | `data[]` (`model`, `displayName`, `isDefault`), `account` |
//! | Cursor | `cursor-agent models`                                      | `<id> - <name> (current, default)` lines               |
//!
//! Nothing here reads another app's credential files or keychain: the CLI answers from its own
//! login. Every child goes through the vendor's [`crate::process::VendorSlot`] (one at a time),
//! and a `claude` child is never killed before [`DetectConfig::claude_kill_grace`].
//!
//! A Ready rail never shows invented rows. Until its CLI answers it is [`RailModels::Loading`]; a
//! failed probe with nothing cached is [`RailModels::Failed`]. The only rows not read from the
//! account are Claude's documented `--model` aliases, used when a CLI build does not answer the
//! handshake at all, and flagged [`SubscriptionModels::documented_aliases`].

mod cache;
mod claude;
mod codex;
mod cursor;

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub use cache::ModelsCache;
pub use claude::DOCUMENTED_ALIASES as CLAUDE_DOCUMENTED_ALIASES;

use crate::locate::DetectConfig;
use crate::model::{Rail, RailState, rail_state};
use crate::probe::{Probe, VendorProbe, probe_all};
use crate::process::RunError;
use crate::status::LoginState;

/// One model the account can use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionModel {
    /// Passed verbatim as the CLI's model flag (`claude --model`, `codex exec -m`,
    /// `cursor-agent --model`).
    pub id: String,
    /// The CLI's own name for it.
    pub label: String,
    /// The CLI marks it as the account's default.
    #[serde(default)]
    pub is_default: bool,
}

/// Who the CLI is signed in as, when it says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// The vendor's plan id as it prints it: Claude `subscriptionType` (`max`, `pro`, …), Codex
    /// `planType` (`plus`, `pro`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

/// A rail's model list as its CLI reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionModels {
    pub rail: Rail,
    /// In the CLI's order, its default first.
    pub models: Vec<SubscriptionModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    /// Claude only: this CLI build did not answer `initialize`, so `models` are the documented
    /// `--model` aliases, not the account's list. Never cached.
    #[serde(default)]
    pub documented_aliases: bool,
    /// Unix seconds when the CLI answered.
    pub fetched_at_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelsError {
    /// The CLI itself says nobody is signed in; the cached list is dropped.
    #[error("not signed in: {0}")]
    NotLoggedIn(String),
    #[error("the CLI did not answer in time")]
    TimedOut,
    /// Another child of this CLI is still running (possibly in its kill grace).
    #[error("another probe of this CLI is still running")]
    Busy,
    #[error("{0}")]
    Failed(String),
}

impl From<RunError> for ModelsError {
    fn from(e: RunError) -> Self {
        match e {
            RunError::Busy(_) => ModelsError::Busy,
            other => ModelsError::Failed(other.to_string()),
        }
    }
}

/// What a vendor module parsed out of its CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    models: Vec<SubscriptionModel>,
    account: Option<Account>,
    documented_aliases: bool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Ask `rail`'s official CLI at `bin` for the account's models, once, within
/// [`DetectConfig::models_timeout`]. Blocking; run it off the UI thread. The caller should have
/// seen the rail Ready ([`crate::status`] is the authority for sign-in; Claude's handshake answers
/// signed out too).
pub fn subscription_models(
    rail: Rail,
    bin: &Path,
    cfg: &DetectConfig,
) -> Result<SubscriptionModels, ModelsError> {
    let env =
        crate::env::minimal_env(&cfg.extra_env).map_err(|e| ModelsError::Failed(e.to_string()))?;
    // The user's home, not Workshop's cwd: the CLI must not pick up the open project's settings
    // or hooks for a model listing.
    let home = cfg.home_dir().filter(|h| h.is_dir());
    let cwd = home.as_deref();
    let answer = match rail {
        Rail::Claude => claude::fetch(bin, cwd, &env, cfg)?,
        Rail::Codex => codex::fetch(bin, cwd, &env, cfg)?,
        Rail::Cursor => cursor::fetch(bin, cwd, &env, cfg)?,
    };
    let mut models = answer.models;
    if let Some(i) = models.iter().position(|m| m.is_default) {
        let default = models.remove(i);
        models.insert(0, default);
    }
    Ok(SubscriptionModels {
        rail,
        models,
        account: answer.account,
        documented_aliases: answer.documented_aliases,
        fetched_at_secs: now_secs(),
    })
}

/// The last good list for `rail` under `cache`, without starting any child.
pub fn cached_subscription_models(rail: Rail, cache: &ModelsCache) -> Option<SubscriptionModels> {
    cache.load(rail)
}

/// Where a rail's model rows stand.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RailModels {
    /// Not installed or not signed in: no model rows (the rail's pill and copy say why).
    #[default]
    NotReady,
    /// Signed in, nothing cached, and the CLI has not answered yet: "Loading models…".
    Loading,
    /// Signed in: the CLI's list. `cached` when it is the last good answer rather than a fresh
    /// one; `error` is why a fresh one could not be had.
    Listed {
        list: SubscriptionModels,
        cached: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Signed in, but the CLI could not list its models and nothing is cached: one "Couldn't load
    /// models" row.
    Failed { reason: String },
}

/// How much [`rail_models`] may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Start no child: the cached list, or [`RailModels::Loading`].
    CacheOnly,
    /// Ask the CLI unless the cached list is younger than `max_age` (`Duration::ZERO` always asks).
    Live { max_age: Duration },
}

/// A list this young is reused by [`Refresh::Live`] instead of asking again (`/model` right
/// after `/auth`, a reopened picker).
pub const FRESH_FOR: Duration = Duration::from_secs(60);

/// One rail's model rows for the picker, given its probe.
///
/// Not Ready → [`RailModels::NotReady`] (an explicit "logged out" also drops the cached list: it
/// belonged to a login that is gone). Ready → the cached list or a live answer per `refresh`; a
/// live success is cached, a "not signed in" answer drops the cache, any other failure falls back
/// to the cached list (any age), else [`RailModels::Failed`].
pub fn rail_models(
    rail: Rail,
    probe: &VendorProbe,
    cfg: &DetectConfig,
    cache: &ModelsCache,
    refresh: Refresh,
) -> RailModels {
    debug_assert_eq!(probe.vendor, rail.vendor());
    let bin = match &probe.binary {
        Some(id) if probe.ready() => id.path.clone(),
        _ => {
            if probe.login == Some(LoginState::LoggedOut) {
                cache.forget(rail);
            }
            return RailModels::NotReady;
        }
    };
    let cached = cache.load(rail);
    let listed = |list, error| RailModels::Listed {
        list,
        cached: true,
        error,
    };
    match (refresh, cached) {
        (Refresh::CacheOnly, Some(list)) => listed(list, None),
        (Refresh::CacheOnly, None) => RailModels::Loading,
        (Refresh::Live { max_age }, Some(list))
            if Duration::from_secs(now_secs().saturating_sub(list.fetched_at_secs)) < max_age =>
        {
            listed(list, None)
        }
        (Refresh::Live { .. }, cached) => match subscription_models(rail, &bin, cfg) {
            Ok(list) => {
                if !list.documented_aliases {
                    cache.store(&list);
                }
                RailModels::Listed {
                    list,
                    cached: false,
                    error: None,
                }
            }
            Err(ModelsError::NotLoggedIn(reason)) => {
                cache.forget(rail);
                RailModels::Failed { reason }
            }
            Err(e) => match cached {
                Some(list) => listed(list, Some(e.to_string())),
                None => RailModels::Failed {
                    reason: e.to_string(),
                },
            },
        },
    }
}

/// [`rail_models`] for all three rails, concurrently (each vendor is still one child at a time).
pub fn rails_models(
    probe: &Probe,
    cfg: &DetectConfig,
    cache: &ModelsCache,
    refresh: Refresh,
) -> [RailModels; 3] {
    std::thread::scope(|s| {
        Rail::ALL
            .map(|rail| {
                s.spawn(move || rail_models(rail, probe.get(rail.vendor()), cfg, cache, refresh))
            })
            .map(|h| h.join().unwrap_or_default())
    })
}

/// The Subscriptions view in one blocking call: probe the CLIs, then each Ready rail's models per
/// `refresh`. Rails come back in picker order.
pub fn picker_rails(cfg: &DetectConfig, cache: &ModelsCache, refresh: Refresh) -> [RailState; 3] {
    let probe = probe_all(cfg);
    let [claude, codex, cursor] = rails_models(&probe, cfg, cache, refresh);
    [
        rail_state(Rail::Claude, &probe.claude, claude),
        rail_state(Rail::Codex, &probe.codex, codex),
        rail_state(Rail::Cursor, &probe.cursor, cursor),
    ]
}
