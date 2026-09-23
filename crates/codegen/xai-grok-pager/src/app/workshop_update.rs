//! Workshop: silent updates. At launch the background updater (`xai_grok_update`) installs a newer
//! release from Workshop's channel and swaps `~/.workshop/bin/workshop` while this version keeps
//! running; the welcome screen offers upstream's restart line meanwhile. The first launch of the
//! new version says so in one line.

use std::path::Path;

/// The version this home last ran, one line in `$WORKSHOP_HOME`.
const MARKER: &str = "last-run-version";

/// Record `running` as the version this home last ran. Returns `running` when the previous launch
/// ran an older version (the updater swapped the binary in between); a first install, the same
/// version, a downgrade, or an unparseable marker say nothing.
pub fn note_launch(home: &Path, running: &str) -> Option<String> {
    let path = home.join(MARKER);
    let previous = std::fs::read_to_string(&path).ok();
    let previous = previous.as_deref().map(str::trim);
    if previous != Some(running) {
        let tmp = home.join(format!(".{MARKER}.tmp.{}", std::process::id()));
        if std::fs::create_dir_all(home).is_ok() && std::fs::write(&tmp, format!("{running}\n")).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    (release_core(running)? > release_core(previous?)?).then(|| running.to_owned())
}

/// `MAJOR.MINOR.PATCH` of a version string, ignoring any pre-release or build suffix.
fn release_core(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let triple = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(triple)
}

/// The one line shown on the first launch after an update.
pub fn updated_line(version: &str) -> String {
    format!("Updated to {version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_launch_after_an_upgrade_says_updated() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(note_launch(home.path(), "0.2.1"), None, "first install");
        assert_eq!(note_launch(home.path(), "0.2.1"), None, "same version again");
        assert_eq!(note_launch(home.path(), "0.2.2"), Some("0.2.2".into()));
        assert_eq!(note_launch(home.path(), "0.2.2"), None, "said once");
        assert_eq!(note_launch(home.path(), "0.2.1"), None, "a downgrade is not an update");
        assert_eq!(
            std::fs::read_to_string(home.path().join(MARKER)).unwrap(),
            "0.2.1\n"
        );
        assert_eq!(updated_line("0.2.3"), "Updated to 0.2.3");
        assert_eq!(note_launch(home.path(), "0.10.0"), Some("0.10.0".into()), "numeric, not lexical");
        assert_eq!(release_core("0.2.3-alpha.1"), Some((0, 2, 3)));
        assert_eq!(release_core("garbage"), None);
    }
}
