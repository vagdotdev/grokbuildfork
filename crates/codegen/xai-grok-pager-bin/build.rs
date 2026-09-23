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

fn main() {
    println!("cargo:rerun-if-env-changed=GROK_VERSION");

    // Watch the git files that change on commit/checkout so the version stamp refreshes
    // Never emit a missing path: cargo treats it as always dirty and rebuilds this crate every build
    let mut watch_paths = Vec::new();
    watch_paths.extend(git_stdout(&["rev-parse", "--git-path", "HEAD"]));
    watch_paths.extend(git_stdout(&["rev-parse", "--git-path", "logs/HEAD"]));
    if let Some(head_ref) = git_stdout(&["symbolic-ref", "-q", "HEAD"]) {
        watch_paths.extend(git_stdout(&["rev-parse", "--git-path", &head_ref]));
    }
    for path in watch_paths.iter().filter(|p| Path::new(p).exists()) {
        println!("cargo:rerun-if-changed={path}");
    }

    let commit = git_stdout(&["rev-parse", "HEAD"])
        .map(|s| s.chars().take(12).collect::<String>())
        .filter(|s| s.len() == 12)
        .unwrap_or_else(|| "unknown".to_string());

    // Workshop: the stamped release version, else the release this source build is built from
    // plus `-dev` (the nearest `v*` tag) — the same rule as xai-grok-version, never the upstream
    // crate version.
    println!("cargo:rerun-if-env-changed=WORKSHOP_VERSION");
    let version = std::env::var("GROK_VERSION")
        .or_else(|_| std::env::var("WORKSHOP_VERSION"))
        .ok()
        .or_else(|| {
            git_stdout(&["describe", "--tags", "--match", "v[0-9]*", "--abbrev=0"])
                .map(|tag| format!("{}-dev", tag.trim_start_matches('v')))
        })
        .unwrap_or_else(|| "0.0.0-dev".to_string());

    println!("cargo:rustc-env=VERSION_WITH_COMMIT={version} ({commit})");

    // grove-projfs imports `ProjectedFSLib.dll`, absent until `Client-ProjFS` is
    // enabled; a load-time import kills startup with STATUS_DLL_NOT_FOUND. Link
    // args from grove-projfs/build.rs do not propagate to this exe.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg=/DELAYLOAD:ProjectedFSLib.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }
}
