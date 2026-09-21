//! First-run default selection, exactly as the plan's "First-run default selection" paragraph:
//!
//! 1. A detected Local server that advertises tools and a ≥ 7B model → `Local · Free · Offline`.
//! 2. Otherwise the free community pool: Kilo Gateway `kilo-auto/free` with fallbacks
//!    `openrouter/free` → `nvidia/nemotron-3-super-120b-a12b:free`, labeled
//!    `Direct API · Free · shared pool · prompts may be logged or used for training · 200 requests/hour per network`.
//! 3. Otherwise (no network for the pool) prompt to connect, offering in order: Sign in with
//!    OpenRouter, Paste a Google AI Studio key, Paste an NVIDIA key, Install Ollama.
//!
//! On 429 or when the user asks for more, the same connect options are offered. OpenCode Zen is
//! never a keyless default; optional xAI stays last and unselected.

use serde::{Deserialize, Serialize};

use crate::catalog::KILO_DEFAULT_CHAIN;
use crate::local::LocalServerStatus;

pub const KILO_DEFAULT_LABEL: &str = "Direct API · Free · shared pool · prompts may be logged or used for training · 200 requests/hour per network";
pub const LOCAL_DEFAULT_LABEL: &str = "Local · Free · Offline";

/// One upgrade path offered after the default (or on 429).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectOption {
    SignInWithOpenRouter,
    PasteGoogleAiStudioKey,
    PasteNvidiaKey,
    InstallOllama,
}

impl ConnectOption {
    /// The plan's order.
    pub const ORDER: [ConnectOption; 4] = [
        ConnectOption::SignInWithOpenRouter,
        ConnectOption::PasteGoogleAiStudioKey,
        ConnectOption::PasteNvidiaKey,
        ConnectOption::InstallOllama,
    ];

    pub fn provider_id(self) -> &'static str {
        match self {
            ConnectOption::SignInWithOpenRouter => "openrouter",
            ConnectOption::PasteGoogleAiStudioKey => "google",
            ConnectOption::PasteNvidiaKey => "nvidia",
            ConnectOption::InstallOllama => "ollama",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ConnectOption::SignInWithOpenRouter => "Sign in with OpenRouter",
            ConnectOption::PasteGoogleAiStudioKey => "Paste a Google AI Studio key",
            ConnectOption::PasteNvidiaKey => "Paste an NVIDIA key",
            ConnectOption::InstallOllama => "Install Ollama",
        }
    }

    /// Deep link the option opens.
    pub fn url(self) -> &'static str {
        match self {
            ConnectOption::SignInWithOpenRouter => "https://openrouter.ai/auth",
            ConnectOption::PasteGoogleAiStudioKey => "https://aistudio.google.com/apikey",
            ConnectOption::PasteNvidiaKey => "https://build.nvidia.com",
            ConnectOption::InstallOllama => "https://ollama.com/download",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DefaultSelection {
    /// A detected local server with a tool-capable ≥ 7B model.
    Local {
        provider_id: String,
        model_id: String,
        /// `provider:model` catalog key.
        key: String,
        label: String,
    },
    /// The keyless community pool with its fallback chain.
    KiloFree {
        primary: String,
        fallbacks: Vec<String>,
        label: String,
        upgrades: Vec<ConnectOption>,
    },
    /// Nothing usable without the user connecting something.
    Prompt { options: Vec<ConnectOption> },
}

impl DefaultSelection {
    pub fn catalog_key(&self) -> Option<String> {
        match self {
            DefaultSelection::Local { key, .. } => Some(key.clone()),
            DefaultSelection::KiloFree { primary, .. } => Some(format!("kilo:{primary}")),
            DefaultSelection::Prompt { .. } => None,
        }
    }
}

/// Pick the first-run default from the local probe results and whether the hosted pool is
/// reachable (`hosted_pool_reachable` = a successful keyless check of Kilo, or simply "online").
pub fn select_default(
    local: &[LocalServerStatus],
    hosted_pool_reachable: bool,
) -> DefaultSelection {
    // Local servers in manifest order; the largest tool-capable model on the first server that has one.
    for status in local.iter().filter(|s| s.is_reachable()) {
        if let Some(model) = status.default_candidate() {
            return DefaultSelection::Local {
                provider_id: status.provider_id.clone(),
                model_id: model.id.clone(),
                key: format!("{}:{}", status.provider_id, model.id),
                label: LOCAL_DEFAULT_LABEL.into(),
            };
        }
    }
    if hosted_pool_reachable {
        return DefaultSelection::KiloFree {
            primary: KILO_DEFAULT_CHAIN[0].into(),
            fallbacks: KILO_DEFAULT_CHAIN[1..]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            label: KILO_DEFAULT_LABEL.into(),
            upgrades: ConnectOption::ORDER.to_vec(),
        };
    }
    DefaultSelection::Prompt {
        options: ConnectOption::ORDER.to_vec(),
    }
}

/// What to offer when the shared pool answers 429 (or the user asks for more).
pub fn on_rate_limited() -> Vec<ConnectOption> {
    ConnectOption::ORDER.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{LocalHealth, LocalModel};

    fn server(id: &str, health: LocalHealth, models: Vec<LocalModel>) -> LocalServerStatus {
        LocalServerStatus {
            provider_id: id.into(),
            base_url: format!("http://127.0.0.1/{id}"),
            health,
            version: None,
            models,
        }
    }

    fn model(id: &str, tools: Option<bool>, params: Option<f64>) -> LocalModel {
        LocalModel {
            id: id.into(),
            tools,
            parameters_b: params,
            context_window: None,
        }
    }

    #[test]
    fn local_tool_capable_7b_wins() {
        let local = vec![
            server(
                "ollama",
                LocalHealth::Reachable,
                vec![
                    model("qwen2.5:0.5b", Some(true), Some(0.5)),
                    model("qwen2.5-coder:7b", Some(true), Some(7.6)),
                ],
            ),
            server("lmstudio", LocalHealth::Unreachable, vec![]),
        ];
        let sel = select_default(&local, true);
        assert_eq!(
            sel,
            DefaultSelection::Local {
                provider_id: "ollama".into(),
                model_id: "qwen2.5-coder:7b".into(),
                key: "ollama:qwen2.5-coder:7b".into(),
                label: LOCAL_DEFAULT_LABEL.into(),
            }
        );
        assert_eq!(
            sel.catalog_key().as_deref(),
            Some("ollama:qwen2.5-coder:7b")
        );
    }

    #[test]
    fn toy_or_toolless_local_models_do_not_preempt_the_pool() {
        let local = vec![server(
            "ollama",
            LocalHealth::Reachable,
            vec![
                model("qwen2.5:0.5b", Some(true), Some(0.5)),
                model("llama3:70b", Some(false), Some(70.0)),
                model("mystery:8b", None, Some(8.0)),
            ],
        )];
        let sel = select_default(&local, true);
        match &sel {
            DefaultSelection::KiloFree {
                primary,
                fallbacks,
                label,
                upgrades,
            } => {
                assert_eq!(primary, "kilo-auto/free");
                assert_eq!(
                    fallbacks,
                    &["openrouter/free", "nvidia/nemotron-3-super-120b-a12b:free"]
                );
                assert_eq!(label, KILO_DEFAULT_LABEL);
                assert_eq!(upgrades, &ConnectOption::ORDER);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(sel.catalog_key().as_deref(), Some("kilo:kilo-auto/free"));
    }

    #[test]
    fn offline_with_nothing_local_prompts_in_plan_order() {
        let sel = select_default(&[], false);
        assert_eq!(
            sel,
            DefaultSelection::Prompt {
                options: vec![
                    ConnectOption::SignInWithOpenRouter,
                    ConnectOption::PasteGoogleAiStudioKey,
                    ConnectOption::PasteNvidiaKey,
                    ConnectOption::InstallOllama,
                ]
            }
        );
        assert_eq!(sel.catalog_key(), None);
        assert_eq!(on_rate_limited(), ConnectOption::ORDER);
        assert_eq!(
            ConnectOption::SignInWithOpenRouter.label(),
            "Sign in with OpenRouter"
        );
        assert_eq!(
            ConnectOption::PasteGoogleAiStudioKey.url(),
            "https://aistudio.google.com/apikey"
        );
    }

    #[test]
    fn zen_and_xai_are_never_defaults() {
        let sel = select_default(&[], true);
        let json = serde_json::to_string(&sel).unwrap();
        assert!(!json.contains("opencode") && !json.contains("xai"));
        for o in ConnectOption::ORDER {
            assert_ne!(o.provider_id(), "opencode");
        }
    }
}
