//! Branding gate: the strings a user meets in menus, themes, settings and notifications say
//! Workshop, never Grok / xAI — except the labeled optional xAI card, which is the one place the
//! word belongs.

use xai_grok_pager::settings::defs::default_settings;
use xai_grok_pager::slash::commands::builtin_commands;
use xai_grok_pager_render::theme::{ThemeKind, display_name_for_canonical};

fn mentions_grok(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("grok") || lower.contains("xai") || lower.contains("x.ai")
}

#[test]
fn themes_are_night_and_day() {
    let names: Vec<&str> = ThemeKind::ALL.iter().map(|k| k.display_name()).collect();
    assert!(names.contains(&"night"), "{names:?}");
    assert!(names.contains(&"day"), "{names:?}");
    for kind in ThemeKind::ALL {
        assert!(
            !mentions_grok(kind.display_name()),
            "theme name {:?} mentions Grok",
            kind.display_name()
        );
        assert!(
            !mentions_grok(display_name_for_canonical(kind.display_name())),
            "theme label {:?} mentions Grok",
            display_name_for_canonical(kind.display_name())
        );
    }
    assert_eq!(display_name_for_canonical("night"), "Night");
    assert_eq!(display_name_for_canonical("day"), "Day");
    // Existing configs keep resolving through hidden aliases.
    assert_eq!(
        ThemeKind::from_name("groknight"),
        Some(ThemeKind::GrokNight)
    );
    assert_eq!(ThemeKind::from_name("grok-day"), Some(ThemeKind::GrokDay));
    assert_eq!(ThemeKind::from_name("night"), Some(ThemeKind::GrokNight));
    assert_eq!(ThemeKind::from_name("day"), Some(ThemeKind::GrokDay));
}

#[test]
fn slash_commands_do_not_mention_grok() {
    let mut names = Vec::new();
    for cmd in builtin_commands() {
        names.push(cmd.name().to_owned());
        for text in [cmd.name(), cmd.description(), cmd.usage()] {
            assert!(!mentions_grok(text), "/{} exposes {text:?}", cmd.name());
        }
    }
    assert!(names.contains(&"model".to_owned()));
    assert!(
        !names.contains(&"models".to_owned()),
        "`/models` is an alias of `/model`, not a second command"
    );
    let model = builtin_commands()
        .into_iter()
        .find(|c| c.name() == "model")
        .expect("/model");
    assert!(model.aliases().contains(&"models"), "{:?}", model.aliases());
}

#[test]
fn settings_labels_and_descriptions_do_not_mention_grok() {
    for def in default_settings() {
        // The one labeled exception: the privacy row that governs the optional xAI card.
        if def.key == "coding_data_sharing" {
            assert!(!mentions_grok(def.label), "{:?}", def.label);
            assert!(
                def.description.contains("optional xAI"),
                "{:?}",
                def.description
            );
            continue;
        }
        assert!(
            !mentions_grok(def.label),
            "setting {:?} label {:?}",
            def.key,
            def.label
        );
        assert!(
            !mentions_grok(def.description),
            "setting {:?} description {:?}",
            def.key,
            def.description
        );
    }
}

/// The system prompt handed to the model on Workshop's own agent loop (Direct API / Local
/// connections) carries no Grok / xAI product identity, for every template audience.
#[test]
fn system_prompt_carries_no_grok_or_xai_identity() {
    use std::collections::HashMap;
    use xai_grok_agent::prompt::context::{PromptAudience, PromptContext, TemplateOverride};
    use xai_grok_tools::types::template_renderer::TemplateRenderer;
    use xai_grok_tools::types::tool::ToolKind;

    let tools: HashMap<ToolKind, String> = [
        (ToolKind::Read, "read_file"),
        (ToolKind::Edit, "search_replace"),
        (ToolKind::Execute, "run_terminal_command"),
        (ToolKind::Search, "grep"),
        (ToolKind::List, "list_dir"),
        (ToolKind::Plan, "todo_write"),
        (ToolKind::Skill, "skill"),
        (ToolKind::BackgroundTaskAction, "get_command_or_subagent_output"),
        (ToolKind::KillTaskAction, "kill_command_or_subagent"),
        (ToolKind::WebSearch, "web_search"),
    ]
    .into_iter()
    .map(|(k, v)| (k, v.to_string()))
    .collect();
    let renderer = TemplateRenderer::new(tools, HashMap::new());

    let primary = PromptContext::default();
    let mut subagent = PromptContext::default();
    subagent.audience = PromptAudience::Subagent;
    let mut codex = PromptContext::default();
    codex.system_prompt = TemplateOverride::Codex;
    for (name, ctx) in [("primary", primary), ("subagent", subagent), ("apply-patch", codex)] {
        let prompt = ctx
            .render_with_renderer(&renderer)
            .unwrap_or_else(|| panic!("{name} prompt renders"));
        assert!(!prompt.is_empty());
        for bad in ["Grok", "grok", "xAI", "x.ai"] {
            let hit = prompt.lines().find(|l| l.contains(bad));
            assert!(
                hit.is_none(),
                "{name} system prompt mentions {bad:?}: {}",
                hit.unwrap_or_default()
            );
        }
        assert!(
            prompt.contains("Workshop"),
            "{name} system prompt names the product"
        );
    }
}
