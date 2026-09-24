//! Spawning through the global process scope, with the one retry every exec of a fresh binary
//! needs.
//!
//! `execve` fails with `ETXTBSY` ("Text file busy") while any process holds the file open for
//! writing. A program that was just written — a vendor CLI the installer put in place, a test's
//! fake — is exactly that for a moment: another thread's child, forked between the write and its
//! own `execve`, still holds the (close-on-exec) write handle. The window is microseconds to a few
//! milliseconds, so a short retry closes it; nothing else is retried.

use std::sync::Arc;
use std::time::Duration;

use xai_tty_utils::ProcessGroup;

/// Attempts before an `ETXTBSY` is reported as the spawn error it is.
const ETXTBSY_ATTEMPTS: u32 = 8;
/// Backoff between attempts, growing linearly (25, 50, … ms).
const ETXTBSY_BACKOFF: Duration = Duration::from_millis(25);

/// [`xai_tty_utils::ProcessScope::spawn`] (prepare, spawn, enroll) that retries `ETXTBSY`.
pub(crate) async fn spawn_enrolled(
    mut cmd: tokio::process::Command,
) -> std::io::Result<(tokio::process::Child, Arc<ProcessGroup>)> {
    let scope = xai_tty_utils::global_process_scope();
    scope.prepare(&mut cmd);
    let mut attempt = 0u32;
    let child = loop {
        // The scope's own `spawn` does prepare + spawn + enroll in one go but consumes the command;
        // retrying needs the three steps apart. The child is enrolled right below.
        #[allow(clippy::disallowed_methods)]
        match cmd.spawn() {
            Ok(child) => break child,
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < ETXTBSY_ATTEMPTS =>
            {
                attempt += 1;
                tracing::debug!(
                    attempt,
                    "spawn hit ETXTBSY (file still open for writing); retrying"
                );
                tokio::time::sleep(ETXTBSY_BACKOFF * attempt).await;
            }
            Err(e) => return Err(e),
        }
    };
    let group = scope.enroll(&child)?;
    Ok((child, group))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A script still open for writing cannot be exec'd (`ETXTBSY`); the spawn waits it out.
    #[tokio::test]
    async fn spawn_waits_out_a_script_still_open_for_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.sh");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"#!/bin/sh\necho ready\n").unwrap();
        file.flush().unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Close the write handle only after the first attempts have failed.
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(file);
        });
        let mut cmd = tokio::process::Command::new(&path);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());
        let (child, _group) = spawn_enrolled(cmd)
            .await
            .expect("spawn succeeds once the writer is gone");
        let out = child.wait_with_output().await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ready");
        closer.join().unwrap();
    }

    /// Any other spawn error is reported at once, untouched.
    #[tokio::test]
    async fn other_errors_are_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = tokio::process::Command::new(dir.path().join("missing"));
        let err = spawn_enrolled(cmd)
            .await
            .err()
            .expect("missing program fails");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
