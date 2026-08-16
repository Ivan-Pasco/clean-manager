//! A stand-in for `clean-runtime` that speaks its CLI contract
//! (PLAN.md §5 Layer B).
//!
//! Manager's `cln run` has to be testable without a real runtime — and without
//! a real component, since manager is not allowed to produce one. This binary
//! honors the argv shape Manager §00.13 step 5 specifies and the guarantees
//! `clean-cli` makes about output, so streaming, exit-code propagation, and
//! path resolution are exercised end-to-end against a real process.
//!
//! Two behaviors are copied from the real binary because manager depends on
//! them:
//!
//! - **stdout carries the guest's bytes and nothing else** (CLIH-10). This
//!   binary writes only what it was told to, with no trailing newline of its
//!   own, so a test can assert byte-exactness.
//! - **The exit code is the guest's** (CLIH-11). It passes through untouched.
//!
//! Behavior is steered by environment variables so one binary covers every
//! case:
//!
//! - `FAKE_RUNTIME_STDOUT=<text>` — write `<text>` to stdout verbatim
//! - `FAKE_RUNTIME_STDERR=<text>` — write `<text>` to stderr
//! - `FAKE_RUNTIME_EXIT=<n>` — exit with `<n>` (default 0)
//! - `FAKE_RUNTIME_ARGV_FILE=<path>` — record received argv, one per line
//! - `FAKE_RUNTIME_CWD_FILE=<path>` — record the working directory it ran in

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    if let Ok(path) = std::env::var("FAKE_RUNTIME_ARGV_FILE") {
        let _ = std::fs::write(&path, argv.join("\n"));
    }
    if let Ok(path) = std::env::var("FAKE_RUNTIME_CWD_FILE") {
        if let Ok(cwd) = std::env::current_dir() {
            let _ = std::fs::write(&path, cwd.to_string_lossy().as_bytes());
        }
    }

    if argv.first().map(String::as_str) == Some("--version") {
        println!("clean-runtime 0.1.0-fake");
        return ExitCode::SUCCESS;
    }

    // The real binary requires a config; CLNH-13 makes an absent one a startup
    // error. Mirroring that here means a manager bug that drops `--config`
    // fails in tests the same way it would in production.
    let has_config = argv.iter().any(|a| a.starts_with("--config"));
    if !has_config {
        eprintln!("error: a host.toml is required");
        return ExitCode::FAILURE;
    }

    // Written with `write_all` rather than `println!` so the bytes reach the
    // pipe exactly as given — no added newline, nothing to make a
    // byte-exactness assertion pass that should not.
    if let Ok(text) = std::env::var("FAKE_RUNTIME_STDOUT") {
        let mut out = std::io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }
    if let Ok(text) = std::env::var("FAKE_RUNTIME_STDERR") {
        let mut err = std::io::stderr();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
    }

    let code: u8 = std::env::var("FAKE_RUNTIME_EXIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    ExitCode::from(code)
}
