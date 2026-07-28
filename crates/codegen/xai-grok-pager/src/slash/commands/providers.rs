//! `/providers` (`/provider`, `/connect`) - secure provider manager.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct ProvidersCommand;

impl SlashCommand for ProvidersCommand {
    fn name(&self) -> &str {
        "providers"
    }

    fn aliases(&self) -> &[&str] {
        &["provider", "connect"]
    }

    fn description(&self) -> &str {
        "Manage API-key and imported model providers"
    }

    fn usage(&self) -> &str {
        "/providers"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenProviderManager)
    }
}
