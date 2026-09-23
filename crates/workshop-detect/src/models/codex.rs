//! Codex: `codex app-server`, JSON-RPC over stdio (methods documented in openai/codex
//! `codex-rs/app-server/README.md`; shapes from `codex app-server generate-json-schema`, pinned
//! against Codex CLI 0.156.1 on 2026-09-23, which answers `model/list` signed out and offline):
//!
//! ```text
//! → {"id":1,"method":"initialize","params":{"clientInfo":{"name":"workshop",…}}}   ← {"id":1,"result":{…}}
//! → {"method":"initialized"}
//! → {"id":2,"method":"account/read","params":{}}
//!     ← {"id":2,"result":{"account":null|{"type":"chatgpt","email":…,"planType":…}|{"type":"apiKey"},"requiresOpenaiAuth":…}}
//! → {"id":3,"method":"model/list","params":{}}
//!     ← {"id":3,"result":{"data":[{"id","model","displayName","isDefault","hidden",…}],"nextCursor":null}}
//! ```
//!
//! `account/read` is sent without `refreshToken`: `true` asks the server to rotate the login's
//! token, which a listing must never do. `model` is the slug `codex exec -m` takes (Traycer,
//! `codex-adapter.ts` `parseCodexModel`, MIT). Closing stdin ends the server.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::time::Instant;

use serde_json::{Value, json};

use super::{Account, Answer, ModelsError, SubscriptionModel};
use crate::locate::DetectConfig;
use crate::model::Vendor;
use crate::process::{Next, Session};

pub(crate) const ARGS: &[&str] = &["app-server"];

/// `model/list` pages followed at most (the server's default page holds the whole list today).
const MAX_PAGES: u64 = 10;

struct Rpc {
    session: Session,
    deadline: Instant,
    early: HashMap<u64, Value>,
}

impl Rpc {
    fn send(&mut self, msg: Value) -> Result<(), ModelsError> {
        self.session
            .send(&msg.to_string())
            .map_err(|e| ModelsError::Failed(format!("codex app-server stdin: {e}")))
    }

    /// The response to request `id` (answers to other ids that arrive first are kept).
    fn response(&mut self, id: u64) -> Result<Value, ModelsError> {
        if let Some(msg) = self.early.remove(&id) {
            return Ok(msg);
        }
        loop {
            match self.session.next_line(self.deadline) {
                Next::Line(line) => {
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    let Some(got) = msg["id"].as_u64() else {
                        continue;
                    };
                    if got == id {
                        return Ok(msg);
                    }
                    self.early.insert(got, msg);
                }
                Next::Eof => {
                    return Err(ModelsError::Failed(
                        "codex app-server exited before answering".into(),
                    ));
                }
                Next::TimedOut => return Err(ModelsError::TimedOut),
            }
        }
    }
}

fn result(msg: Value, method: &str) -> Result<Value, ModelsError> {
    match msg.get("error") {
        Some(err) => Err(ModelsError::Failed(format!(
            "codex {method}: {}",
            err["message"].as_str().unwrap_or("error")
        ))),
        None => Ok(msg["result"].clone()),
    }
}

pub(super) fn fetch(
    bin: &Path,
    cwd: Option<&Path>,
    env: &[(OsString, OsString)],
    cfg: &DetectConfig,
) -> Result<Answer, ModelsError> {
    let (account, models) = exchange(bin, cwd, env, cfg)?;
    Ok(Answer {
        models,
        account: account?,
        documented_aliases: false,
    })
}

type Exchange = (Result<Option<Account>, ModelsError>, Vec<SubscriptionModel>);

/// One app-server session: the account verdict (an explicit "no account" is
/// [`ModelsError::NotLoggedIn`]) and the model list, which the server answers either way.
fn exchange(
    bin: &Path,
    cwd: Option<&Path>,
    env: &[(OsString, OsString)],
    cfg: &DetectConfig,
) -> Result<Exchange, ModelsError> {
    let deadline = Instant::now() + cfg.models_timeout;
    let session = Session::start(
        Vendor::Codex,
        bin,
        ARGS,
        cwd,
        env,
        cfg.models_timeout,
        cfg.kill_grace(Vendor::Codex),
    )?;
    let mut rpc = Rpc {
        session,
        deadline,
        early: HashMap::new(),
    };
    rpc.send(json!({
        "id": 1,
        "method": "initialize",
        "params": { "clientInfo": { "name": "workshop", "title": "Workshop", "version": env!("CARGO_PKG_VERSION") } },
    }))?;
    result(rpc.response(1)?, "initialize")?;
    rpc.send(json!({ "method": "initialized" }))?;
    rpc.send(json!({ "id": 2, "method": "account/read", "params": {} }))?;
    rpc.send(json!({ "id": 3, "method": "model/list", "params": {} }))?;

    // The plan label is optional; only an explicit "no account" fails the listing.
    let account = match result(rpc.response(2)?, "account/read") {
        Ok(r) => interpret_account(&r),
        Err(_) => Ok(None),
    };
    let mut models = Vec::new();
    let mut id = 3;
    loop {
        let (page, cursor) = interpret_model_list(&result(rpc.response(id)?, "model/list")?)?;
        models.extend(page);
        match cursor {
            Some(cursor) if id < 3 + MAX_PAGES => {
                id += 1;
                rpc.send(
                    json!({ "id": id, "method": "model/list", "params": { "cursor": cursor } }),
                )?;
            }
            _ => break,
        }
    }
    Ok((account, models))
}

/// `account/read` result → the ChatGPT plan, `None` for API-key or other providers, and
/// [`ModelsError::NotLoggedIn`] when the server needs an OpenAI login and has none.
pub(crate) fn interpret_account(result: &Value) -> Result<Option<Account>, ModelsError> {
    let account = &result["account"];
    if account.is_null() {
        return if result["requiresOpenaiAuth"].as_bool() == Some(true) {
            Err(ModelsError::NotLoggedIn(
                "codex app-server reports no account".into(),
            ))
        } else {
            Ok(None)
        };
    }
    if account["type"] != "chatgpt" {
        return Ok(None);
    }
    let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    Ok(Some(Account {
        email: text(&account["email"]),
        plan: text(&account["planType"]),
    }))
}

/// `model/list` result → visible models and the next page's cursor.
pub(crate) fn interpret_model_list(
    result: &Value,
) -> Result<(Vec<SubscriptionModel>, Option<String>), ModelsError> {
    let data = result["data"]
        .as_array()
        .ok_or_else(|| ModelsError::Failed("codex model/list returned no data[]".into()))?;
    let models = data
        .iter()
        .filter(|m| m["hidden"].as_bool() != Some(true))
        .filter_map(|m| {
            let id = [&m["model"], &m["id"]]
                .into_iter()
                .find_map(|v| v.as_str().filter(|s| !s.is_empty()))?
                .to_owned();
            Some(SubscriptionModel {
                label: m["displayName"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&id)
                    .to_owned(),
                is_default: m["isDefault"].as_bool() == Some(true),
                id,
            })
        })
        .collect();
    Ok((models, result["nextCursor"].as_str().map(str::to_owned)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codex CLI 0.156.1 `model/list`, signed out (per-model effort/tier fields trimmed).
    const MODEL_LIST: &str = r#"{"data":[{"id":"gpt-6-astra","model":"gpt-6-astra","displayName":"GPT-6-Astra","description":"Frontier intelligence for the most demanding work.","hidden":false,"isDefault":true},{"id":"gpt-6-sol","model":"gpt-6-sol","displayName":"GPT-6-Sol","hidden":false,"isDefault":false},{"id":"gpt-6-luna","model":"gpt-6-luna","displayName":"GPT-6-Luna","hidden":false,"isDefault":false},{"id":"gpt-5.6-sol","model":"gpt-5.6-sol","displayName":"GPT-5.6-Sol","hidden":false,"isDefault":false},{"id":"gpt-5.6-terra","model":"gpt-5.6-terra","displayName":"GPT-5.6-Terra","hidden":false,"isDefault":false},{"id":"gpt-5.6-luna","model":"gpt-5.6-luna","displayName":"GPT-5.6-Luna","hidden":false,"isDefault":false},{"id":"gpt-5.5","model":"gpt-5.5","displayName":"GPT-5.5","hidden":false,"isDefault":false}],"nextCursor":null}"#;

    #[test]
    fn real_model_list_has_seven_models_one_default() {
        let (models, cursor) =
            interpret_model_list(&serde_json::from_str(MODEL_LIST).unwrap()).unwrap();
        assert_eq!(cursor, None);
        assert_eq!(models.len(), 7);
        assert_eq!(models[0].id, "gpt-6-astra");
        assert_eq!(models[0].label, "GPT-6-Astra");
        assert_eq!(models.iter().filter(|m| m.is_default).count(), 1);
        assert!(models[0].is_default);
    }

    #[test]
    fn hidden_models_are_skipped_and_model_slug_wins_over_id() {
        let r = json!({"data":[
            {"id":"row-1","model":"gpt-x","displayName":"GPT-X","hidden":false,"isDefault":false},
            {"id":"secret","model":"secret","displayName":"Secret","hidden":true,"isDefault":false},
        ],"nextCursor":"next"});
        let (models, cursor) = interpret_model_list(&r).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-x");
        assert_eq!(cursor.as_deref(), Some("next"));
        assert!(interpret_model_list(&json!({"items": []})).is_err());
    }

    #[test]
    fn account_read_shapes() {
        // Real signed-out answer (Codex CLI 0.156.1).
        let signed_out = json!({"account":null,"requiresOpenaiAuth":true,"workspaceRouting":null});
        assert!(matches!(
            interpret_account(&signed_out),
            Err(ModelsError::NotLoggedIn(_))
        ));
        let chatgpt = json!({"account":{"type":"chatgpt","email":"user@example.com","planType":"pro"},"requiresOpenaiAuth":true});
        assert_eq!(
            interpret_account(&chatgpt).unwrap(),
            Some(Account {
                email: Some("user@example.com".into()),
                plan: Some("pro".into()),
            })
        );
        let api_key = json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":true});
        assert_eq!(interpret_account(&api_key).unwrap(), None);
        let other_provider = json!({"account":null,"requiresOpenaiAuth":false});
        assert_eq!(interpret_account(&other_provider).unwrap(), None);
    }

    #[test]
    #[ignore = "needs the real Codex CLI on PATH (answers signed out and offline); run with --ignored"]
    fn real_cli_app_server_lists_models_and_reports_the_account() {
        let cfg = DetectConfig::default();
        let Some(id) = crate::probe::probe_vendor(Vendor::Codex, &cfg).binary else {
            eprintln!("codex not installed; skipped");
            return;
        };
        let env = crate::env::minimal_env(&[]).unwrap();
        let home = cfg.home_dir();
        let (account, models) = exchange(&id.path, home.as_deref(), &env, &cfg).unwrap();
        eprintln!("codex {}: account={account:?}", id.version);
        for m in &models {
            eprintln!(
                "  {}{} — {}",
                m.id,
                if m.is_default { " (default)" } else { "" },
                m.label
            );
        }
        assert!(!models.is_empty());
        assert_eq!(models.iter().filter(|m| m.is_default).count(), 1);
        assert!(matches!(
            account,
            Ok(Some(_)) | Err(ModelsError::NotLoggedIn(_))
        ));
    }
}
