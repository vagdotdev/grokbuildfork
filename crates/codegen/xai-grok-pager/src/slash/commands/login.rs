use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand, slash_meta};

/// Workshop: alias of `/auth` (opens the picker on its Subscriptions section; never a browser by
/// default).
pub struct LoginCommand;

impl SlashCommand for LoginCommand {
    slash_meta! {
        name: "login",
        description: "Same as /auth: sign in to Claude, Codex or Cursor, or add an API key",
        usage: "/login",
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::Login)
    }
}
