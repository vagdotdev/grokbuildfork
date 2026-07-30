#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OAuthSupport {
    Direct,
    Grok,
    Pi,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub api_key_env: Option<&'static str>,
    pub oauth: OAuthSupport,
    pub grok_compatible: bool,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "openrouter",
        name: "OpenRouter",
        api_key_env: Some("OPENROUTER_API_KEY"),
        oauth: OAuthSupport::Direct,
        grok_compatible: true,
    },
    Provider {
        id: "xai",
        name: "xAI",
        api_key_env: Some("XAI_API_KEY"),
        oauth: OAuthSupport::Grok,
        grok_compatible: true,
    },
    Provider {
        id: "anthropic",
        name: "Anthropic",
        api_key_env: Some("ANTHROPIC_API_KEY"),
        oauth: OAuthSupport::Pi,
        grok_compatible: true,
    },
    Provider {
        id: "openai",
        name: "OpenAI API",
        api_key_env: Some("OPENAI_API_KEY"),
        oauth: OAuthSupport::None,
        grok_compatible: true,
    },
    Provider {
        id: "openai-codex",
        name: "ChatGPT Plus/Pro (Codex)",
        api_key_env: None,
        oauth: OAuthSupport::Pi,
        grok_compatible: false,
    },
    Provider {
        id: "github-copilot",
        name: "GitHub Copilot",
        api_key_env: Some("COPILOT_GITHUB_TOKEN"),
        oauth: OAuthSupport::Pi,
        grok_compatible: false,
    },
    Provider {
        id: "kimi-coding",
        name: "Kimi Code",
        api_key_env: Some("KIMI_API_KEY"),
        oauth: OAuthSupport::Pi,
        grok_compatible: true,
    },
    Provider {
        id: "radius",
        name: "Radius",
        api_key_env: Some("RADIUS_API_KEY"),
        oauth: OAuthSupport::Pi,
        grok_compatible: false,
    },
];

pub fn find_provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS
        .iter()
        .find(|provider| provider.id.eq_ignore_ascii_case(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_are_unique() {
        let mut ids = std::collections::HashSet::new();
        for provider in PROVIDERS {
            assert!(ids.insert(provider.id));
        }
    }
}
