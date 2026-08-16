//! Spawning the runtime and getting out of the way (Manager §00.13 steps 5–6).
//!
//! # Why this does not use `cln-dispatch::stream`
//!
//! That module pipes the child's stdout so manager can parse a JSON envelope
//! from it. That is right for `cln build`, where stdout is a machine-readable
//! report addressed to manager.
//!
//! It is wrong here. A running guest's stdout belongs to the *user*, not to
//! manager. `clean-cli` guarantees it writes the guest's bytes with no framing
//! of its own (CLIH-10), and manager must add none either — so both streams are
//! inherited outright. The child writes to the real terminal with no pipe in
//! between, which preserves:
//!
//! - **Byte-exactness.** Nothing is decoded, re-encoded, or line-buffered by
//!   us. A guest emitting binary on stdout, or no trailing newline, reaches the
//!   shell as written.
//! - **Interleaving.** stdout and stderr keep their true relative order.
//! - **TTY detection.** The guest sees a terminal when the user has one, so
//!   its own color and progress decisions are correct. Piping stdout would make
//!   every guest believe it was being redirected.
//! - **Interactivity.** stdin is inherited too, so a CLI guest that reads input
//!   works. This is the one place manager inherits stdin rather than closing
//!   it: everywhere else the child is a build tool that should never prompt,
//!   but here the child *is* the user's program.
//!
//! # The exit code is the guest's
//!
//! `clean-cli` returns the guest's own code, or 126 on a trap (CLIH-11,
//! CLIH-14). Manager propagates it verbatim — remapping would make a guest's
//! `exit 3` unobservable, and `cln run` is meant to be as transparent as
//! invoking the binary directly.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, thiserror::Error)]
pub enum InvokeError {
    #[error("could not run the runtime at {}: {source}", .binary.display())]
    Spawn {
        binary: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl InvokeError {
    pub fn remedy(&self) -> Option<String> {
        match self {
            // Almost always a partial extraction or a lost +x bit.
            InvokeError::Spawn { .. } => Some(
                "the install looks damaged; run `cln install runtime latest` to repair it".into(),
            ),
        }
    }
}

/// Everything the runtime needs, assembled by the caller.
#[derive(Clone, Debug)]
pub struct Invocation {
    /// `~/.cln/versions/runtime/<version>/clean-runtime`.
    pub binary: PathBuf,
    /// The world naming which host to spin up.
    pub world: String,
    /// Absolute path to the component.
    pub wasm: PathBuf,
    /// Absolute path to the host configuration.
    pub config: PathBuf,
    /// Static asset directory, for hosts that serve one.
    pub assets: Option<PathBuf>,
    /// Arguments forwarded to the guest after `--`.
    pub guest_args: Vec<String>,
    /// Working directory for the child.
    pub cwd: Option<PathBuf>,
}

impl Invocation {
    /// The argv, exactly as §00.13 step 5 specifies:
    ///
    /// ```text
    /// clean-runtime --world=<world> <wasm> [--assets=<dir>] --config=<path> -- [user args]
    /// ```
    ///
    /// Built as a separate function so the shape is assertable without
    /// spawning anything.
    pub fn argv(&self) -> Vec<OsString> {
        let mut argv: Vec<OsString> = Vec::new();

        argv.push(format!("--world={}", self.world).into());
        argv.push(self.wasm.clone().into_os_string());

        if let Some(assets) = &self.assets {
            let mut flag = OsString::from("--assets=");
            flag.push(assets);
            argv.push(flag);
        }

        let mut config = OsString::from("--config=");
        config.push(&self.config);
        argv.push(config);

        // The separator is emitted only when there is something after it. A
        // bare trailing `--` is harmless to clap but shows up in any argv the
        // runtime logs, so leaving it off keeps the recorded command honest.
        if !self.guest_args.is_empty() {
            argv.push("--".into());
            argv.extend(self.guest_args.iter().map(OsString::from));
        }

        argv
    }
}

/// Run the component and return the runtime's exit code.
///
/// Both output streams and stdin are inherited, so nothing passes through
/// manager. There is no captured output to return — by the time this function
/// returns, everything the guest wrote has already reached the terminal.
pub fn invoke(inv: &Invocation) -> Result<i32, InvokeError> {
    let mut cmd = Command::new(&inv.binary);
    cmd.args(inv.argv())
        // The guest's streams are the user's. See the module comment.
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit());

    if let Some(dir) = &inv.cwd {
        cmd.current_dir(dir);
    }

    let status = cmd.status().map_err(|source| InvokeError::Spawn {
        binary: inv.binary.clone(),
        source,
    })?;

    Ok(status.code().unwrap_or_else(|| signal_code(&status)))
}

/// A process killed by a signal has no exit code. Report the conventional
/// 128+signal so the shell sees a failure rather than a spurious success.
#[cfg(unix)]
fn signal_code(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|s| 128 + s).unwrap_or(1)
}

#[cfg(not(unix))]
fn signal_code(_status: &std::process::ExitStatus) -> i32 {
    1
}

/// Convert a child's exit code into one this process can exit with.
///
/// `ExitCode` is a `u8`; a value outside that range becomes a generic failure
/// rather than wrapping around to 0 and reporting success.
pub fn exit_code(code: i32) -> std::process::ExitCode {
    match u8::try_from(code) {
        Ok(c) => std::process::ExitCode::from(c),
        Err(_) => std::process::ExitCode::FAILURE,
    }
}

/// Build an invocation for an extracted bundle.
///
/// `root` is the extracted archive root; `wasm` and `config` are
/// archive-relative. The child's working directory is the archive root, so a
/// guest that opens a relative path sees the bundle's own layout.
pub fn for_bundle(
    binary: PathBuf,
    root: &Path,
    world: String,
    wasm_relative: &str,
    config_relative: &str,
    guest_args: Vec<String>,
) -> Invocation {
    Invocation {
        binary,
        world,
        // Absolute paths, so the runtime resolves them identically whatever
        // directory the user ran `cln run` from.
        wasm: root.join(wasm_relative),
        config: root.join(config_relative),
        assets: None,
        guest_args,
        cwd: Some(root.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Invocation {
        Invocation {
            binary: PathBuf::from("/cln/versions/runtime/1.0.0/clean-runtime"),
            world: "cli".into(),
            wasm: PathBuf::from("/cache/run/abc/app.wasm"),
            config: PathBuf::from("/cache/run/abc/config/host.toml"),
            assets: None,
            guest_args: Vec::new(),
            cwd: None,
        }
    }

    fn strings(inv: &Invocation) -> Vec<String> {
        inv.argv()
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    /// The exact shape §00.13 step 5 specifies, and the shape verified by hand
    /// against the real runtime.
    #[test]
    fn argv_matches_the_specified_shape() {
        assert_eq!(
            strings(&base()),
            vec![
                "--world=cli",
                "/cache/run/abc/app.wasm",
                "--config=/cache/run/abc/config/host.toml",
            ]
        );
    }

    #[test]
    fn assets_land_between_the_component_and_the_config() {
        let mut inv = base();
        inv.assets = Some(PathBuf::from("/cache/run/abc/assets"));
        assert_eq!(
            strings(&inv),
            vec![
                "--world=cli",
                "/cache/run/abc/app.wasm",
                "--assets=/cache/run/abc/assets",
                "--config=/cache/run/abc/config/host.toml",
            ]
        );
    }

    #[test]
    fn guest_arguments_follow_a_separator() {
        let mut inv = base();
        inv.guest_args = vec!["--verbose".into(), "input.txt".into()];
        let argv = strings(&inv);
        assert_eq!(argv[argv.len() - 3], "--");
        assert_eq!(&argv[argv.len() - 2..], &["--verbose", "input.txt"]);
    }

    /// A guest argument that looks like a runtime flag must be forwarded, not
    /// interpreted — that is what the separator is for.
    #[test]
    fn a_guest_argument_shaped_like_a_flag_is_still_forwarded() {
        let mut inv = base();
        inv.guest_args = vec!["--world=server".into()];
        let argv = strings(&inv);
        assert_eq!(argv[0], "--world=cli", "the runtime's world is unchanged");
        assert_eq!(argv[argv.len() - 2], "--");
        assert_eq!(argv[argv.len() - 1], "--world=server");
    }

    #[test]
    fn no_separator_is_emitted_without_guest_arguments() {
        assert!(!strings(&base()).contains(&"--".to_string()));
    }

    #[test]
    fn a_bundle_invocation_uses_absolute_paths_and_runs_in_the_archive_root() {
        let inv = for_bundle(
            PathBuf::from("/rt/clean-runtime"),
            Path::new("/cache/run/abc"),
            "cli".into(),
            "app.wasm",
            "config/host.toml",
            Vec::new(),
        );

        assert_eq!(inv.wasm, PathBuf::from("/cache/run/abc/app.wasm"));
        assert_eq!(inv.config, PathBuf::from("/cache/run/abc/config/host.toml"));
        assert_eq!(inv.cwd, Some(PathBuf::from("/cache/run/abc")));
    }

    /// A `.serve` bundle's component lives under `wasm/`, not at the root.
    #[test]
    fn a_nested_component_path_resolves_under_the_root() {
        let inv = for_bundle(
            PathBuf::from("/rt/clean-runtime"),
            Path::new("/cache/run/abc"),
            "server".into(),
            "wasm/server.wasm",
            "config/host.toml",
            Vec::new(),
        );
        assert_eq!(inv.wasm, PathBuf::from("/cache/run/abc/wasm/server.wasm"));
    }

    /// A path with a space must survive as one argument. Building argv from
    /// `OsString` rather than a formatted `String` is what guarantees it.
    #[test]
    fn paths_with_spaces_stay_single_arguments() {
        let mut inv = base();
        inv.config = PathBuf::from("/Users/a b/cache/host.toml");
        let argv = inv.argv();
        assert_eq!(argv.len(), 3);
        assert_eq!(
            argv[2].to_string_lossy(),
            "--config=/Users/a b/cache/host.toml"
        );
    }

    #[test]
    fn exit_codes_round_trip_through_the_u8_boundary() {
        use std::process::ExitCode;
        for code in [0u8, 1, 3, 126, 137] {
            assert_eq!(
                format!("{:?}", exit_code(code as i32)),
                format!("{:?}", ExitCode::from(code))
            );
        }
        // Out of range must not wrap to 0 and report success.
        assert_eq!(
            format!("{:?}", exit_code(4096)),
            format!("{:?}", ExitCode::FAILURE)
        );
    }

    #[test]
    fn a_missing_runtime_binary_is_a_spawn_error_with_a_remedy() {
        let tmp = tempfile::tempdir().unwrap();
        let mut inv = base();
        inv.binary = tmp.path().join("nope");

        let err = invoke(&inv).unwrap_err();
        assert!(matches!(err, InvokeError::Spawn { .. }));
        assert!(err.remedy().unwrap().contains("cln install runtime"));
    }
}
