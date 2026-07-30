pub mod openrouter;

use base64::Engine;
use rand::RngExt;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OAuthTokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at_ms: Option<u64>,
    pub extra: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate_pkce() -> Pkce {
    let mut random = [0_u8; 32];
    rand::rng().fill(&mut random);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    Pkce {
        verifier,
        challenge,
    }
}

pub fn parse_authorization_input(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(value)
        && let Some(code) = url.query_pairs().find_map(|(key, value)| {
            (key == "code" && !value.is_empty()).then(|| value.into_owned())
        })
    {
        return Some(code);
    }
    if value.contains("code=")
        && let Some(code) =
            url::form_urlencoded::parse(value.as_bytes()).find_map(|(key, value)| {
                (key == "code" && !value.is_empty()).then(|| value.into_owned())
            })
    {
        return Some(code);
    }
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_uses_url_safe_values() {
        let pkce = generate_pkce();
        assert_eq!(pkce.verifier.len(), 43);
        assert_eq!(pkce.challenge.len(), 43);
        assert!(
            pkce.verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
    }

    #[test]
    fn parses_redirect_url_or_bare_code() {
        assert_eq!(
            parse_authorization_input("http://127.0.0.1/cb?code=a%2Fb&other=x"),
            Some("a/b".to_owned())
        );
        assert_eq!(
            parse_authorization_input("code=hello%20world"),
            Some("hello world".to_owned())
        );
        assert_eq!(
            parse_authorization_input("plain-code"),
            Some("plain-code".to_owned())
        );
        assert_eq!(parse_authorization_input("  "), None);
    }
}
