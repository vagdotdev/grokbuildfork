//! Theme gate: Workshop ships every Grok Build theme. The only changes are the rename
//! Grok Night → Night / Grok Day → Day and the default (a fresh install starts on Oscura
//! Midnight). `/theme` lists all of them on every terminal — macOS Terminal.app is 256-color —
//! and a picked theme renders as itself there instead of being swapped for Night.

use serial_test::serial;
use xai_grok_pager::settings::defs::default_settings;
use xai_grok_pager::settings::registry::SettingKind;
use xai_grok_pager_render::theme::color_support::{self, ColorLevel};
use xai_grok_pager_render::theme::{Theme, ThemeKind, cache, display_name_for_canonical};

/// Upstream `ThemeKind::ALL` (grok-build 1.0.38, `crates/codegen/xai-grok-pager-render/src/theme/mod.rs`),
/// in upstream order, with the rename applied. Upstream's `terminal` theme keeps its upstream rollout
/// gate (`[features] terminal_theme`, off by default), exactly as in Grok Build.
const UPSTREAM_THEMES_RENAMED: &[&str] = &[
    "night",
    "day",
    "tokyonight",
    "rosepine-moon",
    "oscura-midnight",
    "terminal",
];

/// Upstream `THEME_CHOICES` (`crates/codegen/xai-grok-pager/src/settings/defs.rs`) display labels,
/// with the rename applied.
const UPSTREAM_LABELS_RENAMED: &[&str] = &[
    "Auto",
    "Night",
    "Day",
    "Tokyo Night",
    "Rose Pine Moon",
    "Oscura Midnight",
    "Terminal",
];

/// The rename, and nothing else: upstream canonical name → Workshop canonical name.
const RENAMES: &[(&str, &str)] = &[("groknight", "night"), ("grokday", "day")];

/// Pin the process to a 256-color terminal before anything detects the level (the detection is
/// write-once; this file is its own test binary and every test pins first), and reveal the
/// `terminal` theme like upstream's rollout does.
fn pin_256_color() {
    let _ = color_support::set(ColorLevel::Ansi256);
    assert_eq!(
        color_support::detect(),
        ColorLevel::Ansi256,
        "the color level must be pinned before any other detection"
    );
    cache::set_terminal_theme_enabled(true);
}

fn names(kinds: &[ThemeKind]) -> Vec<&'static str> {
    kinds.iter().map(|k| k.display_name()).collect()
}

fn enum_choices(key: &str) -> (Vec<&'static str>, Vec<&'static str>, &'static str) {
    let setting = default_settings()
        .into_iter()
        .find(|s| s.key == key)
        .unwrap_or_else(|| panic!("setting {key:?}"));
    match setting.kind {
        SettingKind::Enum {
            choices, default, ..
        } => (
            choices.iter().map(|c| c.canonical).collect(),
            choices.iter().map(|c| c.display).collect(),
            default,
        ),
        other => panic!("{key:?} is not an enum setting: {other:?}"),
    }
}

#[test]
#[serial]
fn theme_kinds_match_upstream_minus_the_rename() {
    pin_256_color();
    assert_eq!(names(ThemeKind::ALL), UPSTREAM_THEMES_RENAMED);
    // The upstream spellings still resolve, so an existing `theme = "groknight"` keeps working.
    for (upstream, workshop) in RENAMES {
        assert_eq!(
            ThemeKind::from_name(upstream),
            ThemeKind::from_name(workshop),
            "{upstream} must be an alias of {workshop}"
        );
        assert!(
            !UPSTREAM_THEMES_RENAMED.contains(upstream),
            "{upstream} is renamed, not listed"
        );
    }
    for name in UPSTREAM_THEMES_RENAMED {
        let kind = ThemeKind::from_name(name).unwrap_or_else(|| panic!("{name} parses"));
        assert_eq!(kind.display_name(), *name, "canonical name round-trips");
    }
    assert_eq!(ThemeKind::from_name("auto"), Some(ThemeKind::Auto));
}

#[test]
#[serial]
fn settings_theme_catalog_matches_upstream_minus_the_rename() {
    pin_256_color();
    let (canonicals, labels, default) = enum_choices("theme");
    let mut expected = vec!["auto"];
    expected.extend_from_slice(UPSTREAM_THEMES_RENAMED);
    assert_eq!(canonicals, expected, "/settings Theme choices");
    assert_eq!(labels, UPSTREAM_LABELS_RENAMED, "/settings Theme labels");
    assert_eq!(
        default, "oscura-midnight",
        "a fresh install starts on Oscura Midnight"
    );
    for key in ["auto_dark_theme", "auto_light_theme"] {
        let (canonicals, _, _) = enum_choices(key);
        assert_eq!(canonicals, UPSTREAM_THEMES_RENAMED, "{key} choices");
    }
    // The `✓ Theme: …` toast uses the same labels as /settings.
    for (canonical, label) in canonicals.iter().zip(labels.iter()) {
        assert_eq!(display_name_for_canonical(canonical), *label, "toast label");
    }
}

/// The default theme, everywhere it is defined: the startup resolution with no `[ui].theme` and
/// the settings registry's default (what "Reset" restores).
#[test]
#[serial]
fn default_theme_is_oscura_midnight() {
    pin_256_color();
    assert_eq!(ThemeKind::DEFAULT, ThemeKind::OscuraMidnight);
    assert_eq!(ThemeKind::DEFAULT.display_name(), "oscura-midnight");
    assert_eq!(
        display_name_for_canonical("oscura-midnight"),
        "Oscura Midnight"
    );
    let (_, _, default) = enum_choices("theme");
    assert_eq!(default, ThemeKind::DEFAULT.display_name());
    // Auto mode keeps upstream's pair.
    let (_, _, dark) = enum_choices("auto_dark_theme");
    let (_, _, light) = enum_choices("auto_light_theme");
    assert_eq!((dark, light), ("night", "day"));
}

/// What `/theme` lists (`ThemeKind::available()` plus `auto`) on a 256-color terminal: every theme,
/// with the `terminal` theme following its upstream rollout gate.
#[test]
#[serial]
fn theme_picker_lists_every_theme_on_a_256_color_terminal() {
    pin_256_color();
    assert_eq!(names(ThemeKind::available()), UPSTREAM_THEMES_RENAMED);
    assert_eq!(ThemeKind::available(), ThemeKind::selectable());
    cache::set_terminal_theme_enabled(false);
    let gated: Vec<&str> = UPSTREAM_THEMES_RENAMED
        .iter()
        .copied()
        .filter(|n| *n != "terminal")
        .collect();
    assert_eq!(names(ThemeKind::available()), gated);
    cache::set_terminal_theme_enabled(true);
}

/// Picking a truecolor theme on a 256-color terminal keeps that theme (upstream swapped it for
/// Grok Night) and paints its own palette, quantized — never Night's.
#[test]
#[serial]
fn picked_truecolor_theme_renders_as_itself_on_a_256_color_terminal() {
    pin_256_color();
    cache::set_terminal_native_lock(false);
    let night = Theme::groknight().quantized(ColorLevel::Ansi256);
    for (kind, palette) in [
        (ThemeKind::TokyoNight, Theme::tokyonight()),
        (ThemeKind::RosePineMoon, Theme::rosepine_moon()),
        (ThemeKind::OscuraMidnight, Theme::oscura_midnight()),
    ] {
        assert!(kind.requires_truecolor(), "{kind:?} is a truecolor palette");
        assert_eq!(
            Theme::apply_kind(kind),
            kind,
            "{kind:?} is applied as picked"
        );
        assert_eq!(Theme::current_kind(), kind);
        let shown = Theme::current();
        let expected = palette.quantized(ColorLevel::Ansi256);
        if cfg!(windows) {
            assert_eq!(shown.bg_base, expected.bg_base, "{kind:?} background");
        } else {
            assert_eq!(
                shown, expected,
                "{kind:?} renders as its own quantized palette"
            );
        }
        assert_ne!(shown, night, "{kind:?} is not Night in disguise");
    }
    // Night and Day are unaffected.
    assert_eq!(Theme::apply_kind(ThemeKind::GrokDay), ThemeKind::GrokDay);
    assert_eq!(
        Theme::current(),
        Theme::grokday().quantized(ColorLevel::Ansi256)
    );
    assert_eq!(
        Theme::apply_kind(ThemeKind::GrokNight),
        ThemeKind::GrokNight
    );
    assert_eq!(Theme::current(), night);
}
