//! Test-only stand-in `sudo` for the askpass gates.
//!
//! Unlike a shell-script fake, this is a real binary, so the security check that the askpass
//! helper's parent is a live, effective-root process named `sudo` is meaningful: the gate makes
//! this binary setuid-root (`chown root; chmod u+s`), so when the shim execs it, it runs with
//! effective uid 0 and executable name `sudo` — exactly what `effective_root_sudo` looks for.
//!
//! Behaviour mirrors real sudo 1.9.15 run without a terminal: it uses the `SUDO_ASKPASS` helper
//! only when `-A` is given (or `DISPLAY` is set), verifies the password is `hunter2`, then drops
//! privileges and execs the command so any files it creates are owned by the invoking user.

use std::os::unix::process::CommandExt;

fn main() {
    let mut use_askpass = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
    let mut args = std::env::args().skip(1);
    let mut command: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-A" => use_askpass = true,
            "-n" => {
                eprintln!("sudo: a password is required");
                std::process::exit(1);
            }
            "-S" => {
                eprintln!("sudo: this stand-in does not support -S");
                std::process::exit(1);
            }
            "--" => {
                command.extend(args);
                break;
            }
            flag if flag.starts_with('-') => {}
            first => {
                command.push(first.to_string());
                command.extend(args);
                break;
            }
        }
    }

    let askpass = std::env::var_os("SUDO_ASKPASS").filter(|v| !v.is_empty());
    let Some(askpass) = askpass.filter(|_| use_askpass) else {
        eprintln!(
            "sudo: a terminal is required to read the password; either use the -S option to read from standard input or configure an askpass helper"
        );
        eprintln!("sudo: a password is required");
        std::process::exit(1);
    };

    for _ in 0..3 {
        let out = std::process::Command::new(&askpass)
            .arg("[sudo] password for tester: ")
            .output();
        let password = match out {
            Ok(o) => {
                // Forward the helper's stderr (e.g. its "Skipped …" message) as real sudo does, so
                // the model reads why; only stdout carries the password.
                use std::io::Write;
                let _ = std::io::stderr().write_all(&o.stderr);
                if !o.status.success() {
                    eprintln!("sudo: no password was provided");
                    std::process::exit(1);
                }
                String::from_utf8_lossy(&o.stdout).trim_end().to_string()
            }
            Err(_) => {
                eprintln!("sudo: no password was provided");
                std::process::exit(1);
            }
        };
        if password == "hunter2" {
            if command.is_empty() {
                std::process::exit(0);
            }
            // Drop privileges so the command runs as the invoking user (artifacts stay user-owned).
            // SAFETY: getuid/getgid take no arguments; setgid must precede setuid so the gid drop is
            // still permitted.
            unsafe {
                let uid = libc::getuid();
                let gid = libc::getgid();
                libc::setgid(gid);
                libc::setuid(uid);
            }
            let err = std::process::Command::new(&command[0])
                .args(&command[1..])
                .exec();
            eprintln!("sudo: {err}");
            std::process::exit(1);
        }
        eprintln!("Sorry, try again.");
    }
    eprintln!("sudo: 3 incorrect password attempts");
    std::process::exit(1);
}
