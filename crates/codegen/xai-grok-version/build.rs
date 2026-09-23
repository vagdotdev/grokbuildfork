use std::path::Path;
use std::process::Command;

fn git_stdout(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
}

/// Workshop: the version a source build shows is the Workshop release it is built from plus
/// `-dev` (`0.2.1-dev`, from the nearest `v*` tag), never upstream's crate version. The release
/// pipeline stamps the real version through `GROK_VERSION` / `WORKSHOP_VERSION` instead.
fn dev_version() -> String {
    git_stdout(&["describe", "--tags", "--match", "v[0-9]*", "--abbrev=0"])
        .map(|tag| format!("{}-dev", tag.trim_start_matches('v')))
        .unwrap_or_else(|| "0.0.0-dev".to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=GROK_VERSION");
    println!("cargo:rerun-if-env-changed=WORKSHOP_VERSION");
    // The nearest tag changes with checkouts; never emit a missing path (cargo would treat the
    // crate as always dirty).
    for path in git_stdout(&["rev-parse", "--git-path", "HEAD"])
        .into_iter()
        .filter(|p| Path::new(p).exists())
    {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rustc-env=WORKSHOP_DEV_VERSION={}", dev_version());
}
