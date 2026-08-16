//! `cln run` end-to-end, through the real `cln` binary (PLAN.md §5 Layer B).
//!
//! These drive the shipped CLI against `testing/fake-runtime`, which speaks
//! the runtime's argv contract. That is what makes the guarantees below
//! testable without a wasm engine: manager is not allowed to produce a
//! component, so the only honest way to check that it *routes* correctly is to
//! record what the child received.
//!
//! The three properties asserted here are the ones a user would notice
//! immediately if they broke, and that no unit test can prove:
//!
//! - The runtime is invoked with Manager §00.13 step 5's exact argv.
//! - The guest's stdout reaches the shell byte-for-byte (CLIH-10).
//! - The guest's exit code reaches the shell unchanged (CLIH-11).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cln() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cln"))
}

/// The `fake-runtime` binary, built as `clean-runtime` so it lands under the
/// name `cln-layout` expects inside a version directory.
fn fake_runtime() -> PathBuf {
    let mut dir = cln();
    dir.pop();
    let binary = dir.join("clean-runtime");
    assert!(
        binary.is_file(),
        "fake-runtime is not built at {}. `cargo test` builds the binaries of crates it \
         *tests*, and fake-runtime is bin-only, so neither `--workspace` nor `--all-targets` \
         produces it. Run `cargo build -p fake-runtime` first; CI does this in its \
         'build test doubles' step.",
        binary.display()
    );
    binary
}

/// A `~/.cln/` with one runtime installed and active.
fn cln_home(version: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("versions/runtime").join(version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_runtime(), dir.join("clean-runtime")).unwrap();

    // Activate through the real verb, so the test exercises the same symlink
    // code path a user would.
    let out = Command::new(cln())
        .args(["use", "runtime", version])
        .env("CLN_HOME", home.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "`cln use runtime {version}` failed");
    home
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::FileOptions::default();
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
    }
    buf.into_inner()
}

/// A bundle shaped exactly like the one `clean-framework` 0.1.1 produces:
/// component at the root, config one level down reaching back up with
/// `../app.wasm`.
fn clapp(dir: &Path, runtime_version: &str) -> PathBuf {
    let manifest = format!(
        r#"
spec_version = "1"
[package]
name = "hello-world"
version = "0.1.0"
[build]
runtime_version = "{runtime_version}"
[artifact]
kind = "clapp"
worlds = ["cli"]
entry_wasm = "app.wasm"
"#
    );
    let bytes = zip_bytes(&[
        ("manifest.toml", manifest.as_bytes()),
        ("app.wasm", b"\0asm\x0d\x00\x01\x00"),
        (
            "config/host.toml",
            b"[guest]\nwasm = \"../app.wasm\"\nworld = \"cli-default\"\n",
        ),
    ]);
    let path = dir.join("hello-world.clapp");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn run_cln(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(cln());
    cmd.args(args).env("CLN_HOME", home);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("could not run cln")
}

#[test]
fn a_clapp_runs_and_its_stdout_reaches_the_shell_byte_for_byte() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = clapp(tmp.path(), "unknown");

    // No trailing newline: manager must not add one, and must not strip one.
    let out = run_cln(
        home.path(),
        &["run", bundle.to_str().unwrap()],
        &[("FAKE_RUNTIME_STDOUT", "hello")],
    );

    assert_eq!(
        out.stdout, b"hello",
        "stdout must arrive with no framing added by manager (CLIH-10)"
    );
    assert!(out.status.success());
}

/// The success path must be silent. A guest whose stdout is piped into
/// something would otherwise be corrupted by manager's own chatter.
#[test]
fn nothing_is_printed_by_manager_on_a_successful_run() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = clapp(tmp.path(), "unknown");

    let out = run_cln(
        home.path(),
        &["run", bundle.to_str().unwrap()],
        &[("FAKE_RUNTIME_STDOUT", "hello\n")],
    );

    assert!(
        out.stderr.is_empty(),
        "manager wrote to stderr on a clean run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_guests_exit_code_is_propagated_unchanged() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = clapp(tmp.path(), "unknown");

    // 126 is CLIH-14's trap code; 3 is an ordinary guest failure. Both must
    // survive, or a script branching on them silently misreads the result.
    for code in ["0", "1", "3", "42", "126"] {
        let out = run_cln(
            home.path(),
            &["run", bundle.to_str().unwrap()],
            &[("FAKE_RUNTIME_EXIT", code)],
        );
        assert_eq!(
            out.status.code(),
            Some(code.parse().unwrap()),
            "exit code {code} did not survive"
        );
    }
}

/// Manager §00.13 step 5's argv, asserted against what the child actually got.
#[test]
fn the_runtime_receives_the_specified_argv() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = clapp(tmp.path(), "unknown");
    let argv_file = tmp.path().join("argv.txt");

    let out = run_cln(
        home.path(),
        &["run", bundle.to_str().unwrap(), "--", "--loud", "file.txt"],
        &[("FAKE_RUNTIME_ARGV_FILE", argv_file.to_str().unwrap())],
    );
    assert!(out.status.success());

    let argv: Vec<String> = std::fs::read_to_string(&argv_file)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();

    assert_eq!(argv[0], "--world=cli");
    assert!(argv[1].ends_with("/app.wasm"), "got {}", argv[1]);
    assert!(
        argv[2].starts_with("--config=") && argv[2].ends_with("/config/host.toml"),
        "got {}",
        argv[2]
    );
    assert_eq!(&argv[3..], &["--", "--loud", "file.txt"]);
}

/// The load-bearing arrangement. `config/host.toml` says `wasm = "../app.wasm"`
/// and host-core resolves that against the config file's own directory — so if
/// extraction ever flattened the archive, this path would not exist and the
/// bundle would fail at startup for a reason nothing upstream reports.
#[test]
fn extraction_preserves_the_relative_path_the_config_depends_on() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = clapp(tmp.path(), "unknown");
    let argv_file = tmp.path().join("argv.txt");

    run_cln(
        home.path(),
        &["run", bundle.to_str().unwrap()],
        &[("FAKE_RUNTIME_ARGV_FILE", argv_file.to_str().unwrap())],
    );

    let argv = std::fs::read_to_string(&argv_file).unwrap();
    let config = argv
        .lines()
        .find_map(|l| l.strip_prefix("--config="))
        .expect("no --config in argv");

    let from_config = Path::new(config).parent().unwrap().join("../app.wasm");
    assert!(
        from_config.exists(),
        "`../app.wasm` from {config} must reach the component"
    );
}

/// A `.clapp` is normally run from outside any project, so `cln run` must work
/// with no `clean.toml` above the current directory.
#[test]
fn a_bundle_runs_from_a_directory_that_is_not_a_project() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = clapp(tmp.path(), "unknown");

    let out = Command::new(cln())
        .args(["run", bundle.to_str().unwrap()])
        .env("CLN_HOME", home.path())
        .env("FAKE_RUNTIME_STDOUT", "ok")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"ok");
}

/// An artifact pin that is not installed must fail with the install command,
/// not fall back to the active runtime — the component was checked against one
/// host contract and has no guarantee against another.
#[test]
fn an_uninstalled_artifact_pin_fails_with_the_install_command() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = clapp(tmp.path(), "9.9.9");

    let out = run_cln(home.path(), &["run", bundle.to_str().unwrap()], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success());
    assert!(
        stderr.contains("cln install runtime 9.9.9"),
        "the error must name the exact version to install: {stderr}"
    );
}

/// Today's artifacts stamp `runtime_version = "unknown"`; treating that as a
/// pin would make every one of them permanently unrunnable.
#[test]
fn an_unpinned_artifact_uses_the_active_runtime() {
    let home = cln_home("2.5.1");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = clapp(tmp.path(), "unknown");

    let out = run_cln(
        home.path(),
        &["run", bundle.to_str().unwrap(), "--verbose"],
        &[],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("runtime 2.5.1 (active version)"),
        "verbose must report the resolved runtime: {stderr}"
    );
}

/// Deferred rather than broken: the message names the two commands that do the
/// same job today.
#[test]
fn a_project_directory_reports_the_commands_that_work() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("clean.toml"), b"[project]\n").unwrap();

    let out = run_cln(home.path(), &["run", tmp.path().to_str().unwrap()], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success());
    assert!(stderr.contains("cln build"), "{stderr}");
}

/// `--verbose` explains the plan, and must do it on stderr so a piped stdout
/// stays exactly what the guest wrote.
#[test]
fn verbose_output_never_contaminates_stdout() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = clapp(tmp.path(), "unknown");

    let out = run_cln(
        home.path(),
        &["run", bundle.to_str().unwrap(), "--verbose"],
        &[("FAKE_RUNTIME_STDOUT", "guest-output")],
    );

    assert_eq!(out.stdout, b"guest-output");
    assert!(String::from_utf8_lossy(&out.stderr).contains("cln: world cli"));
}

/// A bare component carries no config, and CLNH-13 makes a missing one a
/// startup error — so manager writes the development-defaults file.
#[test]
fn a_bare_wasm_runs_with_a_generated_development_config() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let wasm = tmp.path().join("standalone.wasm");
    std::fs::write(&wasm, b"\0asm\x0d\x00\x01\x00").unwrap();
    let argv_file = tmp.path().join("argv.txt");

    let out = run_cln(
        home.path(),
        &["run", wasm.to_str().unwrap()],
        &[("FAKE_RUNTIME_ARGV_FILE", argv_file.to_str().unwrap())],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let argv = std::fs::read_to_string(&argv_file).unwrap();
    let config = argv
        .lines()
        .find_map(|l| l.strip_prefix("--config="))
        .expect("a generated config must be passed");

    let text = std::fs::read_to_string(config).unwrap();
    assert!(
        text.contains(r#"deployment-mode = "development""#),
        "a generated config must never declare a non-development mode: {text}"
    );
}

#[test]
fn a_missing_artifact_is_reported_by_path() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let ghost = tmp.path().join("ghost.clapp");

    let out = run_cln(home.path(), &["run", ghost.to_str().unwrap()], &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not exist"));
}

/// A truncated download is the common cause of this, and the message should
/// say so rather than leaving the user to guess.
#[test]
fn a_corrupt_bundle_suggests_the_likely_cause() {
    let home = cln_home("1.0.0");
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("bad.clapp");
    std::fs::write(&bad, b"not an archive").unwrap();

    let out = run_cln(home.path(), &["run", bad.to_str().unwrap()], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success());
    assert!(stderr.contains("truncated or corrupt"), "{stderr}");
}
