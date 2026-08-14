//! End-to-end dispatch tests: the real `cln` binary against a fake framework
//! (PLAN.md §5 Layer B).
//!
//! The acceptance criterion for Phase 2: from a clean `~/.cln/`, `cln build
//! <project>` dispatches to the installed framework, streams its diagnostics,
//! and returns its exit code. No compiler is involved — `fake-framework` stands
//! in for the real binary, which is what makes these tests runnable today while
//! `clean-language-compiler` has no published release.
//!
//! Each test builds its own `~/.cln/` in a tempdir and points the binary at it
//! with `CLN_HOME`, so nothing here touches the developer's real toolchain.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// The `cln` binary under test.
fn cln_bin() -> PathBuf {
    // CARGO_BIN_EXE_<name> is set by cargo for integration tests.
    PathBuf::from(env!("CARGO_BIN_EXE_cln"))
}

/// The fake framework, built by the same cargo invocation as this test.
fn fake_framework_bin() -> PathBuf {
    // Sibling of the test binary in target/<profile>/.
    let mut dir = std::env::current_exe().expect("test binary path");
    dir.pop(); // deps/
    if dir.ends_with("deps") {
        dir.pop();
    }
    let bin = dir.join("clean-framework");
    assert!(
        bin.is_file(),
        "fake-framework not built at {}; run `cargo test --workspace`",
        bin.display()
    );
    bin
}

/// A `~/.cln/` with a framework version installed and activated.
struct Toolchain {
    home: TempDir,
}

impl Toolchain {
    fn with_framework(version: &str) -> Self {
        let home = TempDir::new().unwrap();
        let root = home.path();

        let version_dir = root.join("versions").join("framework").join(version);
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::copy(fake_framework_bin(), version_dir.join("clean-framework")).unwrap();

        std::fs::create_dir_all(root.join("active")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&version_dir, root.join("active").join("framework")).unwrap();

        Self { home }
    }

    /// Install another version without activating it — for pin tests.
    fn add_framework(&self, version: &str) -> PathBuf {
        let dir = self
            .home
            .path()
            .join("versions")
            .join("framework")
            .join(version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(fake_framework_bin(), dir.join("clean-framework")).unwrap();
        dir
    }

    fn path(&self) -> &Path {
        self.home.path()
    }
}

/// A project directory containing `clean.toml`.
fn make_project(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("clean.toml"), "[project]\nname = \"demo\"\n").unwrap();
}

/// Pin a toolchain kind for a project.
fn pin(project: &Path, file: &str, version: &str) {
    std::fs::create_dir_all(project.join(".cln")).unwrap();
    std::fs::write(project.join(".cln").join(file), format!("{version}\n")).unwrap();
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run `cln` with `CLN_HOME` pointed at the test toolchain.
fn run_cln(home: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(cln_bin());
    cmd.args(args)
        .env("CLN_HOME", home)
        // Keep assertions free of ANSI escapes.
        .env("NO_COLOR", "1");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("running cln");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// --- the acceptance criterion -------------------------------------------

#[test]
fn build_dispatches_to_the_installed_framework_and_succeeds() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    let run = run_cln(tc.path(), &["build", proj.path().to_str().unwrap()], &[]);

    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    // The framework's own progress reached the terminal.
    assert!(
        run.stderr.contains("fake-framework: build starting"),
        "framework progress should stream to stderr: {}",
        run.stderr
    );
    // Manager rendered the envelope rather than dumping raw JSON.
    assert!(
        run.stderr.contains("dist/app.wasm"),
        "expected a rendered summary: {}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("\"status\""),
        "the raw envelope must not leak into human output: {}",
        run.stderr
    );
}

#[test]
fn a_failing_build_streams_diagnostics_and_propagates_exit_1() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    let run = run_cln(
        tc.path(),
        &["build", proj.path().to_str().unwrap()],
        &[("FAKE_FRAMEWORK_FAIL", "1")],
    );

    assert_eq!(run.code, 1, "the framework's exit code must survive");
    // The component's own diagnostic text reached the terminal live.
    assert!(
        run.stderr.contains("CFG005"),
        "the diagnostic should reach the user: {}",
        run.stderr
    );
    assert!(run.stderr.contains("invalid UTF-8 in clean.toml"));
    // Manager adds the outcome summary on top of it.
    assert!(run.stderr.contains("build failed"));
}

/// The framework prints its diagnostics to stderr itself, so manager's default
/// output must not repeat them — one error, shown once.
#[test]
fn diagnostics_are_not_printed_twice() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    let run = run_cln(
        tc.path(),
        &["build", proj.path().to_str().unwrap()],
        &[("FAKE_FRAMEWORK_FAIL", "1")],
    );

    assert_eq!(
        run.stderr.matches("CFG005").count(),
        1,
        "the diagnostic should appear exactly once: {}",
        run.stderr
    );
}

/// `--verbose` opts into manager's structured rendering, which adds the notes
/// and helps carried in the envelope.
#[test]
fn verbose_renders_the_structured_diagnostic() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    let run = run_cln(
        tc.path(),
        &["build", proj.path().to_str().unwrap(), "--verbose"],
        &[("FAKE_FRAMEWORK_FAIL", "1")],
    );

    assert!(
        run.stderr.contains("note: the file must be UTF-8"),
        "notes come from the envelope: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("help: re-save the file as UTF-8"),
        "helps come from the envelope: {}",
        run.stderr
    );
}

#[test]
fn exit_code_two_survives_dispatch() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    // 2 is the framework's "invoked wrongly"; CI branches on it.
    let run = run_cln(
        tc.path(),
        &["build", proj.path().to_str().unwrap()],
        &[("FAKE_FRAMEWORK_EXIT", "2")],
    );
    assert_eq!(run.code, 2);
}

// --- pin resolution ------------------------------------------------------

#[test]
fn a_project_pin_overrides_the_active_framework() {
    let tc = Toolchain::with_framework("0.1.1");
    tc.add_framework("0.2.0");

    let proj = TempDir::new().unwrap();
    make_project(proj.path());
    pin(proj.path(), "frame-version", "0.2.0");

    let run = run_cln(
        tc.path(),
        &["build", proj.path().to_str().unwrap(), "--verbose"],
        &[],
    );

    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("framework 0.2.0"),
        "the pinned version should be chosen: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("project pin"),
        "--verbose should say the pin won: {}",
        run.stderr
    );
}

#[test]
fn without_a_pin_the_active_version_is_used() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    let run = run_cln(
        tc.path(),
        &["build", proj.path().to_str().unwrap(), "--verbose"],
        &[],
    );
    assert!(run.stderr.contains("active version"), "{}", run.stderr);
}

#[test]
fn a_pin_to_an_uninstalled_version_names_the_install_command() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());
    pin(proj.path(), "frame-version", "9.9.9");

    let run = run_cln(tc.path(), &["build", proj.path().to_str().unwrap()], &[]);

    assert_ne!(run.code, 0);
    assert!(
        run.stderr.contains("cln install framework 9.9.9"),
        "the error should name the fix: {}",
        run.stderr
    );
}

/// The compiler pin is the framework's to resolve, not manager's.
#[test]
fn a_compiler_pin_does_not_affect_framework_selection() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());
    pin(proj.path(), "version", "7.7.7");

    let run = run_cln(
        tc.path(),
        &["build", proj.path().to_str().unwrap(), "--verbose"],
        &[],
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(run.stderr.contains("framework 0.1.1"), "{}", run.stderr);
}

// --- argv forwarding -----------------------------------------------------

#[test]
fn flags_are_forwarded_to_the_framework() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());
    let argv_file = proj.path().join("argv.txt");

    let run = run_cln(
        tc.path(),
        &[
            "build",
            proj.path().to_str().unwrap(),
            "--offline",
            "--override",
            "build.optimization=size",
        ],
        &[("FAKE_FRAMEWORK_ARGV_FILE", argv_file.to_str().unwrap())],
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let argv: Vec<String> = std::fs::read_to_string(&argv_file)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();

    assert_eq!(argv[0], "build", "the verb leads, with no `cln` prefix");
    assert!(argv.contains(&"--offline".to_string()));
    assert!(argv.contains(&"--override".to_string()));
    assert!(argv.contains(&"build.optimization=size".to_string()));
}

#[test]
fn package_forwards_its_own_verb_and_renders_the_package() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());
    let argv_file = proj.path().join("argv.txt");

    let run = run_cln(
        tc.path(),
        &["package", proj.path().to_str().unwrap()],
        &[("FAKE_FRAMEWORK_ARGV_FILE", argv_file.to_str().unwrap())],
    );

    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(std::fs::read_to_string(&argv_file)
        .unwrap()
        .starts_with("package"));
    assert!(
        run.stderr.contains("dist/app.clapp"),
        "the package path should render: {}",
        run.stderr
    );
}

#[test]
fn build_runs_from_the_project_root_when_invoked_from_a_subdirectory() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());
    let nested = proj.path().join("src").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    let argv_file = proj.path().join("argv.txt");

    let run = run_cln(
        tc.path(),
        &["build", nested.to_str().unwrap()],
        &[("FAKE_FRAMEWORK_ARGV_FILE", argv_file.to_str().unwrap())],
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    // The framework receives the discovered root, not the nested path.
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    let forwarded = argv.lines().nth(1).unwrap();
    assert_eq!(
        std::fs::canonicalize(forwarded).unwrap(),
        std::fs::canonicalize(proj.path()).unwrap()
    );
}

// --- output modes --------------------------------------------------------

#[test]
fn json_mode_emits_the_raw_envelope_on_stdout() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    let run = run_cln(
        tc.path(),
        &["build", proj.path().to_str().unwrap(), "--json"],
        &[],
    );

    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let parsed: serde_json::Value = serde_json::from_str(run.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout should be one JSON envelope ({e}): {}", run.stdout));
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["dist_wasm"], "dist/app.wasm");
}

#[test]
fn human_mode_keeps_stdout_clean_for_piping() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    let run = run_cln(tc.path(), &["build", proj.path().to_str().unwrap()], &[]);
    assert!(
        run.stdout.trim().is_empty(),
        "human output belongs on stderr; stdout was: {}",
        run.stdout
    );
}

// --- failures before dispatch --------------------------------------------

#[test]
fn a_directory_without_clean_toml_is_reported_as_not_a_project() {
    let tc = Toolchain::with_framework("0.1.1");
    let empty = TempDir::new().unwrap();

    let run = run_cln(tc.path(), &["build", empty.path().to_str().unwrap()], &[]);
    assert_ne!(run.code, 0);
    assert!(
        run.stderr.contains("no Clean project found"),
        "{}",
        run.stderr
    );
}

#[test]
fn no_framework_installed_names_the_install_command() {
    // A clean ~/.cln/ with nothing in it.
    let home = TempDir::new().unwrap();
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    let run = run_cln(home.path(), &["build", proj.path().to_str().unwrap()], &[]);
    assert_ne!(run.code, 0);
    assert!(
        run.stderr.contains("cln install framework latest"),
        "a first-run user needs the install command: {}",
        run.stderr
    );
}

#[test]
fn a_malformed_pin_is_reported_rather_than_silently_ignored() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());
    pin(proj.path(), "frame-version", "not-a-version");

    let run = run_cln(tc.path(), &["build", proj.path().to_str().unwrap()], &[]);
    assert_ne!(run.code, 0, "must not fall back to the active version");
    assert!(run.stderr.contains("frame-version"), "{}", run.stderr);
}

#[test]
fn verbs_the_framework_lacks_are_not_offered() {
    let tc = Toolchain::with_framework("0.1.1");
    let proj = TempDir::new().unwrap();
    make_project(proj.path());

    // `dev` and `new` are PLAN Phase 2/5 verbs with no counterpart in
    // clean-framework 0.1.1. Until it ships them, clap rejects them here
    // rather than producing a bare exit-2 from the component.
    for verb in ["dev", "new"] {
        let run = run_cln(tc.path(), &[verb, proj.path().to_str().unwrap()], &[]);
        assert_ne!(run.code, 0, "`{verb}` should not be dispatched yet");
        assert!(
            run.stderr.contains("unrecognized subcommand")
                || run.stderr.contains("unexpected argument"),
            "expected a clap rejection for `{verb}`: {}",
            run.stderr
        );
    }
}
