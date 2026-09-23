//! Picker data model: rails, pills, model rows, and per-rail state.

use serde::{Deserialize, Serialize};

use crate::copy;
use crate::models::RailModels;
use crate::probe::VendorProbe;
use crate::status::LoginState;

/// A detectable official CLI. OpenCode is detected (and can be spawned as an adapter) but is not a
/// subscription rail in the picker; the picker shows it on the Models tab as a catalog provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    Claude,
    Codex,
    Cursor,
    OpenCode,
}

impl Vendor {
    pub const ALL: [Vendor; 4] = [
        Vendor::Claude,
        Vendor::Codex,
        Vendor::Cursor,
        Vendor::OpenCode,
    ];

    /// Stable id used in connection registries and logs.
    pub fn id(self) -> &'static str {
        match self {
            Vendor::Claude => "claude",
            Vendor::Codex => "codex",
            Vendor::Cursor => "cursor",
            Vendor::OpenCode => "opencode",
        }
    }

    /// User-visible product name.
    pub fn display_name(self) -> &'static str {
        match self {
            Vendor::Claude => "Claude Code",
            Vendor::Codex => "Codex",
            Vendor::Cursor => "Cursor Agent",
            Vendor::OpenCode => "OpenCode",
        }
    }

    /// Executable names to look for, in preference order. Every candidate still has to pass
    /// [`crate::identify`]; the name alone proves nothing.
    pub fn binary_names(self) -> &'static [&'static str] {
        match self {
            Vendor::Claude => &["claude"],
            Vendor::Codex => &["codex"],
            // The current Cursor installer links both names to the same binary. `agent` is a
            // generic word, so it is verified strictly.
            Vendor::Cursor => &["cursor-agent", "agent"],
            Vendor::OpenCode => &["opencode"],
        }
    }
}

/// Subscription rails, in the order the picker shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rail {
    Claude,
    Codex,
    Cursor,
}

impl Rail {
    pub const ALL: [Rail; 3] = [Rail::Claude, Rail::Codex, Rail::Cursor];

    pub fn vendor(self) -> Vendor {
        match self {
            Rail::Claude => Vendor::Claude,
            Rail::Codex => Vendor::Codex,
            Rail::Cursor => Vendor::Cursor,
        }
    }

    /// Rail label as shown next to the logo.
    pub fn display_name(self) -> &'static str {
        match self {
            Rail::Claude => "Claude",
            Rail::Codex => "Codex",
            Rail::Cursor => "Cursor",
        }
    }

    /// Catalog provider id the rail's model rows use (`harnessProviderID` in the export).
    pub fn provider_id(self) -> &'static str {
        match self {
            Rail::Claude => "anthropic",
            Rail::Codex => "openai",
            Rail::Cursor => "cursor",
        }
    }
}

/// The single status pill on a rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pill {
    Detecting,
    Ready,
    SignIn,
    /// The official CLI is not installed: Enter runs the vendor's installer, then its sign-in.
    Install,
}

impl Pill {
    pub fn label(self) -> &'static str {
        match self {
            Pill::Detecting => "Detecting",
            Pill::Ready => "Ready",
            Pill::SignIn => "Sign in",
            Pill::Install => "Install",
        }
    }
}

/// One selectable model radio. The key is `provider:model:variant` so that two variants of the same
/// model (for example Cursor Fast and Max) are separate rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Human-readable name; falls back to the model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            variant: None,
            display_name: None,
        }
    }

    pub fn with_variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = Some(variant.into());
        self
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// Radio key: `provider:model:variant` (variant omitted when absent).
    pub fn key(&self) -> String {
        match &self.variant {
            Some(v) => format!("{}:{}:{}", self.provider, self.model, v),
            None => format!("{}:{}", self.provider, self.model),
        }
    }

    pub fn display(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.model)
    }
}

/// Everything the picker needs to draw one rail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RailState {
    pub rail: Rail,
    pub pill: Pill,
    /// A verified vendor binary was found.
    pub installed: bool,
    /// Models shown as radios on the right: the CLI's own list, in its order, default first.
    pub models: Vec<ModelRef>,
    /// Copy shown when `models` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_copy: Option<&'static str>,
    /// Whether the rail shows a Connect button.
    pub show_connect: bool,
    /// Where `models` came from (loading, live, cached, failed; plan and email), for Ready rails.
    #[serde(default)]
    pub subscription: RailModels,
}

impl RailState {
    /// State while the probe is still running.
    pub fn detecting(rail: Rail) -> Self {
        Self {
            rail,
            pill: Pill::Detecting,
            installed: false,
            models: Vec::new(),
            empty_copy: None,
            show_connect: false,
            subscription: RailModels::NotReady,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.pill == Pill::Ready
    }
}

/// Composer label for a selected rail model: `Claude · {model}`.
pub fn composer_label(rail: Rail, model: &ModelRef) -> String {
    format!("{} · {}", rail.display_name(), model.display())
}

/// Build one rail's picker state from its probe result and its model rows
/// ([`crate::models::rail_models`]).
///
/// Rules, matching the export:
/// * pill is Ready only when a verified binary is installed **and** the official status command
///   reports signed in; installed but signed out (or unknown) is Sign in; not installed is
///   Install (Enter runs the vendor's official installer, then its sign-in);
/// * models are shown only on a Ready rail, and only as its CLI listed them: a Ready rail that is
///   still loading or failed to load shows that copy, never placeholder rows;
/// * Connect shows when the rail is not ready, or when its CLI listed no models (except Cursor).
pub fn rail_state(rail: Rail, probe: &VendorProbe, models: RailModels) -> RailState {
    debug_assert_eq!(probe.vendor, rail.vendor());
    let installed = probe.binary.is_some();
    let ready = installed && matches!(probe.login, Some(LoginState::LoggedIn));
    let subscription = if ready { models } else { RailModels::NotReady };
    let rows: Vec<ModelRef> = match &subscription {
        RailModels::Listed { list, .. } => list
            .models
            .iter()
            .map(|m| ModelRef::new(rail.provider_id(), &m.id).with_display_name(&m.label))
            .collect(),
        _ => Vec::new(),
    };
    let empty_copy = rows.is_empty().then(|| match &subscription {
        RailModels::Loading => copy::LOADING_MODELS,
        RailModels::Failed { .. } => copy::MODELS_FAILED,
        _ => copy::empty_rail_copy(rail, installed, ready, probe.app_present),
    });
    let show_connect = !matches!(
        subscription,
        RailModels::Loading | RailModels::Failed { .. }
    ) && copy::needs_connect(rail, ready, rows.is_empty());

    RailState {
        rail,
        pill: if ready {
            Pill::Ready
        } else if installed {
            Pill::SignIn
        } else {
            Pill::Install
        },
        installed,
        models: rows,
        empty_copy,
        show_connect,
        subscription,
    }
}

/// Build all three rails in picker order.
pub fn rails(
    probe: &crate::probe::Probe,
    models_for: impl Fn(Rail) -> RailModels,
) -> [RailState; 3] {
    [
        rail_state(Rail::Claude, &probe.claude, models_for(Rail::Claude)),
        rail_state(Rail::Codex, &probe.codex, models_for(Rail::Codex)),
        rail_state(Rail::Cursor, &probe.cursor, models_for(Rail::Cursor)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_order_is_claude_codex_cursor() {
        assert_eq!(Rail::ALL, [Rail::Claude, Rail::Codex, Rail::Cursor]);
        assert_eq!(
            Rail::ALL.map(Rail::display_name),
            ["Claude", "Codex", "Cursor"]
        );
    }

    #[test]
    fn pill_labels_match_export() {
        assert_eq!(Pill::Detecting.label(), "Detecting");
        assert_eq!(Pill::Ready.label(), "Ready");
        assert_eq!(Pill::SignIn.label(), "Sign in");
        assert_eq!(Pill::Install.label(), "Install");
    }

    #[test]
    fn model_key_separates_variants() {
        let fast = ModelRef::new("cursor", "composer").with_variant("fast");
        let max = ModelRef::new("cursor", "composer").with_variant("max");
        assert_eq!(fast.key(), "cursor:composer:fast");
        assert_eq!(max.key(), "cursor:composer:max");
        assert_ne!(fast.key(), max.key());
        assert_eq!(ModelRef::new("anthropic", "opus").key(), "anthropic:opus");
    }

    #[test]
    fn composer_label_uses_middle_dot() {
        let m = ModelRef::new("anthropic", "opus").with_display_name("Claude Opus");
        assert_eq!(composer_label(Rail::Claude, &m), "Claude · Claude Opus");
        assert_eq!(
            composer_label(Rail::Cursor, &ModelRef::new("cursor", "auto")),
            "Cursor · auto"
        );
    }

    #[test]
    fn cursor_binary_names_verify_generic_agent_last() {
        assert_eq!(Vendor::Cursor.binary_names(), &["cursor-agent", "agent"]);
    }
}
