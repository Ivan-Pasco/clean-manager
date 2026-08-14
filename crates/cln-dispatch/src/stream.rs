//! Spawning a component binary and relaying its output.
//!
//! **Why the two streams are handled differently.** The component's stderr is
//! human-readable progress and must reach the terminal *as it happens* — a
//! thirty-second build that prints nothing until it finishes looks hung. Its
//! stdout is a single machine-readable envelope meant for manager, not for the
//! user, so it is captured and parsed rather than echoed (PLAN.md §3).
//!
//! stderr is therefore inherited outright: the child writes to the real
//! terminal with no pipe and no relay thread in between, which preserves
//! interleaving, TTY detection, and color decisions the child makes for itself.
//! Only stdout is piped.
//!
//! **The exit code is the component's.** Manager adds no interpretation: the
//! framework's `0` / `1` / `2` reach the shell unchanged, so scripts and CI can
//! branch on them exactly as if they had invoked the component directly.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What a dispatched process produced.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// The exit code, to be propagated verbatim.
    pub code: i32,
    /// Captured stdout — the JSON envelope, unparsed.
    pub stdout: String,
    /// The binary that ran, for diagnostics.
    pub binary: PathBuf,
}

impl Outcome {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("could not run {}: {source}", .binary.display())]
    Spawn {
        binary: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not read output from {}: {source}", .binary.display())]
    Io {
        binary: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl DispatchError {
    pub fn remedy(&self) -> Option<String> {
        match self {
            // Almost always a partial extraction or a lost +x bit.
            DispatchError::Spawn { .. } => {
                Some("the install looks damaged; re-run `cln install` for this component".into())
            }
            DispatchError::Io { .. } => None,
        }
    }
}

/// Run `binary` with `args`, streaming stderr live and capturing stdout.
///
/// `cwd` sets the child's working directory when given. Extra environment
/// variables are applied on top of the inherited environment.
pub fn dispatch<S: AsRef<OsStr>>(
    binary: &Path,
    args: &[S],
    cwd: Option<&Path>,
    env: &[(&str, String)],
) -> Result<Outcome, DispatchError> {
    let mut cmd = Command::new(binary);
    cmd.args(args.iter().map(AsRef::as_ref))
        // Piped: parsed by manager, never shown raw.
        .stdout(Stdio::piped())
        // Inherited: the user's progress feed, live and unbuffered by us.
        .stderr(Stdio::inherit())
        // The child reads no input; giving it a closed stdin means a component
        // that unexpectedly prompts fails fast instead of hanging the terminal.
        .stdin(Stdio::null());

    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }

    let output = cmd.output().map_err(|source| DispatchError::Spawn {
        binary: binary.to_path_buf(),
        source,
    })?;

    // `output()` waits for exit and collects the piped stdout. stderr is not
    // captured here because it was inherited and has already reached the
    // terminal.
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    Ok(Outcome {
        // A process killed by a signal has no exit code; report the
        // conventional 128+signal so the shell sees a nonzero failure rather
        // than a spurious success.
        code: output
            .status
            .code()
            .unwrap_or_else(|| signal_code(&output.status)),
        stdout,
        binary: binary.to_path_buf(),
    })
}

#[cfg(unix)]
fn signal_code(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|s| 128 + s).unwrap_or(1)
}

#[cfg(not(unix))]
fn signal_code(_status: &std::process::ExitStatus) -> i32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// A shell script standing in for a component binary.
    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    #[cfg(unix)]
    fn captures_stdout_and_propagates_success() {
        let tmp = tempdir().unwrap();
        let bin = script(tmp.path(), "fake", r#"echo '{"status":"ok"}'"#);

        let out = dispatch(&bin, &[] as &[&str], None, &[]).unwrap();
        assert_eq!(out.code, 0);
        assert!(out.success());
        assert_eq!(out.stdout.trim(), r#"{"status":"ok"}"#);
    }

    #[test]
    #[cfg(unix)]
    fn propagates_a_nonzero_exit_code_verbatim() {
        let tmp = tempdir().unwrap();
        // Exit 2 is the framework's "invoked wrongly" — it must survive.
        let bin = script(tmp.path(), "fake", "exit 2");

        let out = dispatch(&bin, &[] as &[&str], None, &[]).unwrap();
        assert_eq!(out.code, 2);
        assert!(!out.success());
    }

    #[test]
    #[cfg(unix)]
    fn forwards_arguments_in_order() {
        let tmp = tempdir().unwrap();
        let bin = script(tmp.path(), "fake", r#"echo "$@""#);

        let out = dispatch(&bin, &["build", "./app", "--offline"], None, &[]).unwrap();
        assert_eq!(out.stdout.trim(), "build ./app --offline");
    }

    #[test]
    #[cfg(unix)]
    fn runs_in_the_requested_directory() {
        let tmp = tempdir().unwrap();
        let bin = script(tmp.path(), "fake", "pwd");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();

        let out = dispatch(&bin, &[] as &[&str], Some(&work), &[]).unwrap();
        assert_eq!(
            std::fs::canonicalize(out.stdout.trim()).unwrap(),
            std::fs::canonicalize(&work).unwrap()
        );
    }

    #[test]
    #[cfg(unix)]
    fn passes_environment_variables_through() {
        let tmp = tempdir().unwrap();
        let bin = script(tmp.path(), "fake", r#"echo "$CLN_OFFLINE""#);

        let out = dispatch(&bin, &[] as &[&str], None, &[("CLN_OFFLINE", "1".into())]).unwrap();
        assert_eq!(out.stdout.trim(), "1");
    }

    #[test]
    fn a_missing_binary_is_a_spawn_error_with_a_remedy() {
        let tmp = tempdir().unwrap();
        let err = dispatch(&tmp.path().join("nope"), &[] as &[&str], None, &[]).unwrap_err();
        assert!(matches!(err, DispatchError::Spawn { .. }));
        assert!(err.remedy().unwrap().contains("cln install"));
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_stdout_does_not_panic() {
        let tmp = tempdir().unwrap();
        let bin = script(tmp.path(), "fake", r#"printf '\xff\xfe'"#);

        let out = dispatch(&bin, &[] as &[&str], None, &[]).unwrap();
        assert_eq!(out.code, 0);
        assert!(!out.stdout.is_empty(), "lossy conversion keeps the bytes");
    }

    #[test]
    #[cfg(unix)]
    fn a_signal_killed_child_reports_failure_not_success() {
        let tmp = tempdir().unwrap();
        // SIGKILL leaves no exit code; a naive unwrap_or(0) would read as success.
        let bin = script(tmp.path(), "fake", "kill -9 $$");

        let out = dispatch(&bin, &[] as &[&str], None, &[]).unwrap();
        assert!(!out.success(), "a killed build must not look successful");
        assert_eq!(out.code, 128 + 9);
    }
}
