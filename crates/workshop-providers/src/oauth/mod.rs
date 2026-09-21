//! Workshop-owned OAuth flows, only for providers that document one for third-party clients.
//! Today that is OpenRouter's PKCE key-minting flow. No vendor CLI OAuth client id is ever reused.

pub mod openrouter;

pub use openrouter::{
    CallbackServer, OpenRouterSignIn, PkceError, PkcePair, SignInMode, authorize_url,
    exchange_code, parse_callback_request, parse_pasted_code,
};
