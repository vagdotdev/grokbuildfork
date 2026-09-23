use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand, slash_meta};

pub struct ReleaseNotesCommand;

/// Title of the release-notes document (the welcome menu row and `/release-notes` share it).
pub const RELEASE_NOTES_TITLE: &str = "Release Notes";

/// The release-notes document to show: the fetched markdown when a changelog fetch produced one,
/// else Workshop's bundled notes. Never "(offline)": the bundled notes are always there.
pub fn release_notes_document(fetched_markdown: Option<String>) -> (String, String) {
    let content = fetched_markdown
        .map(|md| md.trim().to_owned())
        .filter(|md| !md.is_empty())
        .unwrap_or_else(|| workshop_brand::RELEASE_NOTES.trim().to_owned());
    (RELEASE_NOTES_TITLE.to_owned(), content)
}

impl SlashCommand for ReleaseNotesCommand {
    slash_meta! {
        name: "release-notes",
        aliases: ["changelog"],
        description: "View release notes for the current version",
        usage: "/release-notes",
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        let changelog = xai_grok_shell::util::changelog::ChangelogManager::new().fetch();
        let (title, content) = release_notes_document(changelog.markdown);
        CommandResult::Action(Action::ShowReleaseNotes { title, content })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_notes_metadata() {
        let cmd = ReleaseNotesCommand;
        assert_eq!(cmd.name(), "release-notes");
        assert_eq!(cmd.aliases(), &["changelog"]);
        assert!(!cmd.takes_args());
    }

    #[test]
    fn release_notes_fall_back_to_the_bundled_notes() {
        let models = crate::acp::model_state::ModelState::default();
        let mut ctx = super::super::tests::make_ctx(&models);
        match ReleaseNotesCommand.run(&mut ctx, "") {
            CommandResult::Action(Action::ShowReleaseNotes { title, content }) => {
                assert_eq!(title, RELEASE_NOTES_TITLE);
                assert!(content.contains("# Workshop release notes"), "{content}");
            }
            other => panic!("expected the release notes to open, got {other:?}"),
        }
    }

    #[test]
    fn fetched_markdown_wins_over_the_bundle_when_present() {
        let (_, fetched) = release_notes_document(Some("  # Fetched\n".into()));
        assert_eq!(fetched, "# Fetched");
        let (_, empty) = release_notes_document(Some("   ".into()));
        assert!(empty.starts_with("# Workshop release notes"));
        let (_, none) = release_notes_document(None);
        assert_eq!(none, empty);
    }
}
