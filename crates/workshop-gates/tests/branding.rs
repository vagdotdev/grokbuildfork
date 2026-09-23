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
        (
            ToolKind::BackgroundTaskAction,
            "get_command_or_subagent_output",
        ),
        (ToolKind::KillTaskAction, "kill_command_or_subagent"),
        (ToolKind::WebSearch, "web_search"),
    ]
    .into_iter()
    .map(|(k, v)| (k, v.to_string()))
    .collect();
    let renderer = TemplateRenderer::new(tools, HashMap::new());

    let primary = PromptContext::default();
    let subagent = PromptContext {
        audience: PromptAudience::Subagent,
        ..Default::default()
    };
    let codex = PromptContext {
        system_prompt: TemplateOverride::Codex,
        ..Default::default()
    };
    for (name, ctx) in [
        ("primary", primary),
        ("subagent", subagent),
        ("apply-patch", codex),
    ] {
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

/// `workshop --help` (and every subcommand's help) talks about Workshop: no `grok` command,
/// no `~/.grok`, no `GROK_*` environment names. The labeled optional xAI sign-in is the one
/// place the other product may be named.
#[test]
fn cli_help_names_workshop_not_grok() {
    use clap::CommandFactory;
    fn check(cmd: &mut clap::Command, path: &str) {
        let help = cmd.render_long_help().to_string();
        for line in help.lines() {
            let lower = line.to_ascii_lowercase();
            if !(lower.contains("grok") || lower.contains("xai") || lower.contains("x.ai")) {
                continue;
            }
            let allowed = line.contains("optional xAI")
                || line.contains("xAI account")
                || line.contains("--xai")
                || line.contains("auth.x.ai")
                || line.contains("grokbuildfork")
                || line.contains("./.grok/");
            assert!(
                allowed,
                "`{path} --help` mentions the other product: {line:?}"
            );
        }
        let subs: Vec<String> = cmd
            .get_subcommands()
            .filter(|c| !c.is_hide_set())
            .map(|c| c.get_name().to_owned())
            .collect();
        for name in subs {
            let sub = cmd.find_subcommand_mut(&name).expect("subcommand exists");
            check(sub, &format!("{path} {name}"));
        }
    }
    let mut cmd = xai_grok_pager::app::cli::PagerArgs::command();
    check(&mut cmd, "workshop");
    let help = cmd.render_long_help().to_string();
    assert!(
        help.contains("~/.workshop"),
        "help names the Workshop home:\n{help}"
    );
    assert!(
        help.contains("WORKSHOP_SANDBOX"),
        "the sandbox env var carries the Workshop name"
    );
    assert!(
        help.contains("WORKSHOP_AGENT_DASHBOARD"),
        "the dashboard env var carries the Workshop name"
    );
}

/// The `/tutorial` pages are Workshop's: titles, blurbs and bodies never name the other product
/// or its `grok` command.
#[test]
fn tutorial_names_workshop_not_grok() {
    let topics = xai_grok_pager::tutorial_docs::TUTORIAL_TOPICS;
    assert!(topics.len() >= 8);
    for topic in topics {
        for (what, text) in [
            ("title", topic.title),
            ("blurb", topic.blurb),
            ("body", topic.content),
        ] {
            for line in text.lines() {
                assert!(
                    !mentions_grok(line),
                    "tutorial {:?} {what} mentions the other product: {line:?}",
                    topic.title
                );
            }
        }
    }
    let first_prompt = topics
        .iter()
        .find(|t| t.title == "Your First Prompt")
        .expect("first-prompt page");
    assert!(
        first_prompt
            .content
            .starts_with("# Your First Prompt\n\nWorkshop is a conversation")
    );
}

/// The bundled `/docs` guide describes Workshop: the index, every title and description, and the
/// page bodies carry no Grok / xAI remnant beyond a short allowlist of real identifiers the
/// binary still uses (ACP `x.ai/*` method names, crate names, the project-level `.grok/`
/// directory, `/etc/grok` managed-config paths, the optional xAI account).
#[test]
fn docs_guide_names_workshop_not_grok() {
    let allowed = |line: &str| {
        line.contains("x.ai/")
            || line.contains("grokbuildfork")
            || line.contains("grok-build\"")
            || line.contains("`grok-build`")
            || line.contains("grok-cli")
            || line.contains("grok_com_")
            || line.contains(".grok-plugin/")
            || line.contains("refs/grok/")
            || line.contains("grok_code.")
            || line.contains("xai-grok-")
            || line.contains(".grok/")
            || line.contains("/etc/grok")
            || line.contains("xai_api_base_url")
            || line.contains("WORKSHOP_XAI")
            || line.contains("xAI")
            || line.contains("auth.x.ai")
    };
    let guide = xai_grok_pager::docs::USER_GUIDE;
    assert!(guide.len() >= 20, "{} guides", guide.len());
    for doc in guide {
        assert!(!mentions_grok(doc.title), "guide title {:?}", doc.title);
        assert!(
            !mentions_grok(doc.description),
            "guide {:?} description {:?}",
            doc.title,
            doc.description
        );
        assert!(
            !doc.filename.contains("grok"),
            "guide file {:?}",
            doc.filename
        );
        for line in doc.content.lines() {
            if mentions_grok(line) && !allowed(line) {
                panic!("guide {:?} mentions the other product: {line:?}", doc.title);
            }
        }
    }
    assert!(
        !guide
            .iter()
            .any(|d| d.title == "grok clone" || d.title.contains("OpenTelemetry")),
        "the xAI-only pages are gone"
    );
    let index = xai_grok_pager::docs::find_doc("Getting Started").expect("Getting Started");
    for wrong in ["no default model provider", "no public install channel"] {
        assert!(
            !index.content.contains(wrong),
            "Getting Started still says {wrong:?}"
        );
    }
    for right in [
        "curl -fsSL https://raw.githubusercontent.com/vagdotdev/grokbuildfork/release-channel/install.sh | sh",
        "OpenCode · Big Pickle",
        "`/model`",
        "`/auth`",
    ] {
        assert!(
            index.content.contains(right),
            "Getting Started lacks {right:?}"
        );
    }
    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../codegen/xai-grok-pager/docs/user-guide/README.md"),
    )
    .expect("guide index");
    assert!(readme.starts_with("# Workshop User Guide"));
    for line in readme.lines() {
        assert!(
            !mentions_grok(line) || line.contains("optional xAI"),
            "guide index mentions the other product: {line:?}"
        );
    }
}

/// The welcome card: one product name on every launch, an invitation to type in the composer,
/// and bundled release notes for its "Release notes" row.
#[test]
fn welcome_copy_is_one_name_and_an_invitation_to_type() {
    assert_eq!(workshop_brand::title(), "Vagdev's Workshop");
    assert_eq!(workshop_brand::title(), workshop_brand::TITLE);
    assert!(workshop_brand::PROMPT_PLACEHOLDER.starts_with("Ask anything"));
    assert!(workshop_brand::PROMPT_PLACEHOLDER.contains("\"add a test for multiply\""));
    assert!(!mentions_grok(&workshop_brand::hero_subtitle()));
    assert!(workshop_brand::RELEASE_NOTES.starts_with("# Workshop release notes"));
    // The notes may name the repository and, once, `~/.grok` as the directory Workshop refuses
    // to read; nothing else.
    for line in workshop_brand::RELEASE_NOTES.lines() {
        assert!(
            !mentions_grok(line) || line.contains("grokbuildfork") || line.contains("`~/.grok`"),
            "release notes mention the other product: {line:?}"
        );
    }
}

/// Progressive disclosure: the bare `/` menu leads with eight common commands and parks the
/// power tools under `advanced`; every name in both lists is a real command.
#[test]
fn slash_menu_groups_are_common_then_advanced() {
    use xai_grok_pager::slash::{ADVANCED_COMMANDS, COMMON_COMMANDS, is_advanced_command};
    let names: Vec<String> = builtin_commands()
        .into_iter()
        .map(|c| c.name().to_owned())
        .collect();
    assert_eq!(COMMON_COMMANDS.len(), 8);
    for common in COMMON_COMMANDS {
        assert!(
            names.contains(&common.to_string()),
            "common command /{common} exists"
        );
        assert!(!is_advanced_command(common));
    }
    // Some advanced verbs are shell-provided or feature-gated; the built-in ones must be real.
    let known_advanced = ADVANCED_COMMANDS
        .iter()
        .filter(|a| names.contains(&a.to_string()))
        .count();
    assert!(
        known_advanced >= 15,
        "{known_advanced} of {} advanced commands are built in",
        ADVANCED_COMMANDS.len()
    );
    for advanced in ADVANCED_COMMANDS {
        assert!(
            !COMMON_COMMANDS.contains(advanced),
            "/{advanced} cannot be both"
        );
    }
    for everyday in [
        "model", "auth", "help", "docs", "tutorial", "feedback", "theme", "quit", "new", "resume",
        "compact", "context",
    ] {
        assert!(
            !is_advanced_command(everyday),
            "/{everyday} is not advanced"
        );
    }
}
