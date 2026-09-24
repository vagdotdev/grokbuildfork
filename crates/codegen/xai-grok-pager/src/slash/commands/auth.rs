//! Workshop overlay: `/auth` opens the connection picker on its Subscriptions section (the same
//! list `/model` opens; `/models` is an alias of `/model`).
//!
//! Neither command starts a login. The picker overlay is the only default auth surface; the
//! optional xAI row inside it is the single path to the inherited xAI OIDC flow.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand, slash_meta};

pub struct AuthCommand;

impl SlashCommand for AuthCommand {
    slash_meta! {
        name: "auth",
        description: "Sign in to Claude, Codex or Cursor, or add an API key",
        usage: "/auth",
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenConnectionPicker(
            workshop_auth::PickerFocus::Subscriptions,
        ))
    }
}
