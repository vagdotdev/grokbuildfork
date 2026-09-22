//! Workshop overlay: `/auth` opens the Subscriptions view (`/models` is an alias of `/model`).
//!
//! Neither command starts a login. The picker overlay is the only default auth surface; the
//! optional xAI card inside it is the single path to the inherited xAI OIDC flow.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand, slash_meta};

pub struct AuthCommand;

impl SlashCommand for AuthCommand {
    slash_meta! {
        name: "auth",
        description: "Connect a subscription CLI (Claude, Codex, Cursor) or an API key",
        usage: "/auth",
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenConnectionPicker(
            workshop_auth::PickerTab::Subscriptions,
        ))
    }
}
