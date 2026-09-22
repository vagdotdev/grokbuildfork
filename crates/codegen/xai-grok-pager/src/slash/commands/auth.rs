//! Workshop overlay: `/auth` and `/models` open the connection picker.
//!
//! Neither command starts a login. The picker is the only default auth surface; the optional xAI
//! card inside it is the single path to the inherited xAI OIDC flow.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand, slash_meta};

pub struct AuthCommand;

impl SlashCommand for AuthCommand {
    slash_meta! {
        name: "auth",
        description: "Open the connection picker (Local, API key, subscription CLI, optional xAI)",
        usage: "/auth",
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenConnectionPicker(
            workshop_auth::PickerTab::Models,
        ))
    }
}

pub struct ModelsCommand;

impl SlashCommand for ModelsCommand {
    slash_meta! {
        name: "models",
        description: "Browse model connections: Models and Subscriptions tabs",
        usage: "/models",
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenConnectionPicker(
            workshop_auth::PickerTab::Models,
        ))
    }
}
