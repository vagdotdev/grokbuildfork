//! Canary isolation: a secret saved for provider A is never released for provider B's host, for a
//! LAN host, for a plaintext non-loopback URL, or across a redirect target.

use std::sync::Arc;

use workshop_providers::{
    CredentialBroker, MemorySecretStore, ProviderError, catalog::custom_row, sampler_config_for,
};

fn broker(tmp: &tempfile::TempDir) -> CredentialBroker {
    CredentialBroker::new(
        Arc::new(MemorySecretStore::default()),
        tmp.path().join("home/connections.json"),
    )
}

#[test]
fn canary_a_never_reaches_host_b() {
    let tmp = tempfile::tempdir().unwrap();
    let broker = broker(&tmp);
    broker.save_api_key("openai", "CANARY-OPENAI").unwrap();
    broker
        .save_api_key("anthropic", "CANARY-ANTHROPIC")
        .unwrap();
    broker
        .save_api_key("openrouter", "CANARY-OPENROUTER")
        .unwrap();
    broker.save_api_key("opencode", "CANARY-ZEN").unwrap();

    let providers = ["openai", "anthropic", "openrouter", "opencode"];
    let hosts = [
        "https://api.openai.com/v1/responses",
        "https://api.anthropic.com/v1/messages",
        "https://openrouter.ai/api/v1/chat/completions",
        "https://opencode.ai/zen/v1/chat/completions",
        "https://api.x.ai/v1/chat/completions",
        "https://evil.example.com/v1/chat/completions",
        "http://127.0.0.1:11434/v1/chat/completions",
    ];
    for (i, provider) in providers.iter().enumerate() {
        let handle = broker.resolve(provider).unwrap();
        for (j, url) in hosts.iter().enumerate() {
            let result = handle.authorize(url);
            if i == j {
                let value = result.unwrap().unwrap();
                assert!(value.starts_with("CANARY-"), "{provider} → {url}");
            } else {
                match result {
                    Err(ProviderError::HostNotAllowed { provider: p, .. }) => {
                        assert_eq!(&p, provider)
                    }
                    other => panic!("{provider} credential leaked toward {url}: {other:?}"),
                }
            }
        }
    }
}

#[test]
fn lookalike_and_subdomain_hosts_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let broker = broker(&tmp);
    broker.save_api_key("openai", "CANARY").unwrap();
    let handle = broker.resolve("openai").unwrap();
    for url in [
        "https://api.openai.com.evil.example/v1",
        "https://evil.example/api.openai.com/v1",
        "https://user@api.openai.com.attacker.example/",
        "https://eu.api.openai.com/v1",
        "http://api.openai.com/v1",
    ] {
        assert!(
            handle.authorize(url).is_err(),
            "{url} must not receive the credential"
        );
    }
    // Case-insensitive host match is fine; that is still the allowed host.
    assert_eq!(
        handle
            .authorize("https://API.OpenAI.com/v1/responses")
            .unwrap(),
        Some("CANARY")
    );
}

#[test]
fn sampler_config_never_mixes_provider_and_credential() {
    let tmp = tempfile::tempdir().unwrap();
    let broker = broker(&tmp);
    broker.save_api_key("openai", "CANARY-OPENAI").unwrap();
    broker
        .save_api_key("anthropic", "CANARY-ANTHROPIC")
        .unwrap();
    broker.add_local("vllm").unwrap();
    let openai = broker.resolve("openai").unwrap();
    let anthropic = broker.resolve("anthropic").unwrap();
    let vllm = broker.resolve("vllm").unwrap();

    let anthropic_model = custom_row("anthropic", "claude-sonnet-4-5").unwrap();
    let vllm_model = custom_row("vllm", "meta-llama/Llama-3").unwrap();

    assert!(sampler_config_for(&anthropic_model, &openai).is_err());
    assert!(sampler_config_for(&vllm_model, &openai).is_err());
    assert!(sampler_config_for(&vllm_model, &anthropic).is_err());
    let ok = sampler_config_for(&anthropic_model, &anthropic).unwrap();
    assert_eq!(ok.api_key.as_deref(), Some("CANARY-ANTHROPIC"));
    let local = sampler_config_for(&vllm_model, &vllm).unwrap();
    assert_eq!(local.api_key, None, "local rows carry no credential at all");

    // The serialized config for the local model contains no canary.
    let dumped = serde_json::to_string(&local).unwrap();
    assert!(!dumped.contains("CANARY"));
}

#[test]
fn env_presence_is_reported_without_the_value() {
    let tmp = tempfile::tempdir().unwrap();
    let broker = broker(&tmp);
    // SAFETY: unique to this test binary.
    unsafe { std::env::set_var("OPENROUTER_API_KEY", "CANARY-ENV") };
    assert!(broker.env_key_present("openrouter").unwrap());
    assert!(!broker.env_key_present("ollama").unwrap());
    // Nothing was written anywhere by checking presence.
    assert!(!broker.connections_path().exists());
    assert!(broker.connections().unwrap().connections.is_empty());

    // "Use without saving" records the variable name only.
    broker.use_env_key("openrouter").unwrap();
    let text = std::fs::read_to_string(broker.connections_path()).unwrap();
    assert!(text.contains("OPENROUTER_API_KEY"));
    assert!(!text.contains("CANARY-ENV"));
    let handle = broker.resolve("openrouter").unwrap();
    assert_eq!(
        handle
            .authorize("https://openrouter.ai/api/v1/chat/completions")
            .unwrap(),
        Some("CANARY-ENV")
    );
    assert!(
        handle
            .authorize("https://api.openai.com/v1/responses")
            .is_err()
    );
    unsafe { std::env::remove_var("OPENROUTER_API_KEY") };
}
