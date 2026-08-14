//! A stand-in for `clean-framework` that speaks its CLI contract with canned
//! responses (PLAN.md §5 Layer B).
//!
//! Manager's dispatch has to be testable without a real framework — and today
//! it *must* be, because the real framework resolves a compiler that has no
//! published release yet. This binary honors the same contract manager relies
//! on, so dispatch, streaming, exit-code propagation, and envelope rendering
//! are all exercised end-to-end against something real:
//!
//! - `build` / `package` verbs, everything else exits 2
//! - human progress on stderr, one JSON envelope on stdout
//! - exit 0 success, 1 build failed with diagnostics, 2 invoked wrongly
//!
//! Behavior is steered by environment variables so a test can ask for a
//! failure without a different binary:
//!
//! - `FAKE_FRAMEWORK_FAIL=1` — emit an error envelope and exit 1
//! - `FAKE_FRAMEWORK_EXIT=<n>` — exit with `<n>` regardless
//! - `FAKE_FRAMEWORK_STDOUT=<text>` — emit `<text>` instead of an envelope
//! - `FAKE_FRAMEWORK_ARGV_FILE=<path>` — record received argv, one per line

use std::io::Write;
use std::process::ExitCode;

const VERSION: &str = "0.1.1-fake";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    if let Ok(path) = std::env::var("FAKE_FRAMEWORK_ARGV_FILE") {
        let _ = std::fs::write(&path, argv.join("\n"));
    }

    let verb = match argv.first().map(String::as_str) {
        Some(v) => v,
        None => {
            eprintln!("error: a verb is required");
            return ExitCode::from(2);
        }
    };

    if verb == "--version" {
        println!("clean-framework {VERSION}");
        return ExitCode::SUCCESS;
    }

    if !matches!(verb, "build" | "package") {
        // Matches clap's behavior in the real binary.
        eprintln!("error: unrecognized subcommand '{verb}'");
        return ExitCode::from(2);
    }

    // Progress goes to stderr, exactly as the real framework does.
    eprintln!("fake-framework: {verb} starting");
    let _ = std::io::stderr().flush();

    if let Ok(raw) = std::env::var("FAKE_FRAMEWORK_STDOUT") {
        println!("{raw}");
        return ExitCode::SUCCESS;
    }

    if let Ok(code) = std::env::var("FAKE_FRAMEWORK_EXIT") {
        return ExitCode::from(code.parse::<u8>().unwrap_or(1));
    }

    if std::env::var("FAKE_FRAMEWORK_FAIL").is_ok() {
        eprintln!("error[CFG005]: invalid UTF-8 in clean.toml");
        let envelope = format!(
            r#"{{"status":"error","diagnostics":[{{"level":"error","code":"CFG005","message":"invalid UTF-8 in clean.toml","notes":["the file must be UTF-8"],"helps":["re-save the file as UTF-8"]}}],"framework_version":"{VERSION}"}}"#
        );
        println!("{envelope}");
        return ExitCode::from(1);
    }

    let envelope = match verb {
        "package" => format!(
            r#"{{"status":"ok","package":"dist/app.clapp","package_sha256":"cafe","kind":"clapp","rebuilt":false,"framework_version":"{VERSION}"}}"#
        ),
        _ => format!(
            r#"{{"status":"ok","dist_wasm":"dist/app.wasm","build_manifest":"dist/build-manifest.json","request_sha256":"beef","wasm_sha256":"f00d","diagnostics":[],"framework_version":"{VERSION}"}}"#
        ),
    };
    eprintln!("fake-framework: {verb} done");
    println!("{envelope}");
    ExitCode::SUCCESS
}
