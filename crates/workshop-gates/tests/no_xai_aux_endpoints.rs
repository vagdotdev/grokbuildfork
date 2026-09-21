//! Gate 3, auxiliary services (ADR 0004): every inherited xAI destination that is *not* in
//! `PRODUCTION_ENDPOINTS` must still be loopback by default. These were found by the hermetic
//! login proof, not by grep: the computer-hub supervisor URL and the remote share / permissions
//! backend each carried their own grok.com literal.

use workshop_gates::{url_hits_forbidden_host, url_is_loopback};

#[test]
fn computer_hub_default_is_loopback() {
    let url = xai_grok_shell_base::env::PROD_COMPUTER_HUB_WS_URL;
    assert!(
        !url_hits_forbidden_host(url),
        "PROD_COMPUTER_HUB_WS_URL still dials xAI infrastructure: {url}"
    );
    assert!(url_is_loopback(url), "PROD_COMPUTER_HUB_WS_URL = {url}");
}

#[test]
#[serial_test::serial]
fn remote_share_backend_default_is_loopback() {
    // SAFETY: serial test; the variable is restored below.
    let prev = std::env::var_os("GROK_CODE_BACKEND_URL");
    unsafe { std::env::remove_var("GROK_CODE_BACKEND_URL") };
    let base = xai_grok_shell::remote::client::BackendClient::new()
        .base_url()
        .to_string();
    unsafe {
        match prev {
            Some(v) => std::env::set_var("GROK_CODE_BACKEND_URL", v),
            None => std::env::remove_var("GROK_CODE_BACKEND_URL"),
        }
    }
    assert!(
        !url_hits_forbidden_host(&base),
        "remote share backend still defaults to xAI infrastructure: {base}"
    );
    assert!(url_is_loopback(&base), "remote share backend = {base}");
}

#[test]
fn operator_override_of_remote_backend_is_honoured() {
    // Not a default: an explicit GROK_CODE_BACKEND_URL is deployment configuration.
    let client =
        xai_grok_shell::remote::client::BackendClient::with_base_url("https://code.example.test");
    assert_eq!(client.base_url(), "https://code.example.test");
}
