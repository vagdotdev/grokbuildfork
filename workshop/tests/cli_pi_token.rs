#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn pi_token_command_emits_only_grok_credential_json() {
    let temp = tempfile::tempdir().unwrap();
    let pi = temp.path().join("pi");
    std::fs::write(
        &pi,
        "#!/bin/sh\n[ \"$1\" = \"auth\" ] || exit 9\nprintf 'pi-bearer-token\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o700)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_workshop"))
        .args([
            "auth",
            "token",
            "anthropic",
            "--source",
            "pi",
            "--model",
            "claude-test",
        ])
        .env("WORKSHOP_PI_BINARY", &pi)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["access_token"], "pi-bearer-token");
    assert_eq!(json["expires_in"], 25 * 60);
}
