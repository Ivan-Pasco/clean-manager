//! The small window a double-click opens (Manager §00.12, P-3).
//!
//! **Why AppleScript rather than a GUI toolkit.** P-3 requires "a small
//! native-feeling window with no runtime dependency a user must install
//! first", and names the toolkit as deferred. `osascript` is already on every
//! macOS machine, renders real system dialogs, and adds nothing to the binary.
//! A toolkit crate would add build weight and a second rendering story for
//! three buttons and a field list. When Windows and Linux registration lands,
//! each gets its own implementation behind this same interface.
//!
//! **Nothing here interpolates user data into a script.** Every value that
//! comes from a package — its name, description, path — reaches AppleScript as
//! an argument (`on run argv`), never by string concatenation. A package named
//! `" & do shell script "rm -rf ~` would otherwise be a code-execution bug in
//! the one place explicitly meant to protect someone opening an untrusted
//! file.

use std::path::Path;
use std::process::ExitCode;

use cln_layout::Layout;
use cln_run::Inspection;

/// What the user chose in the open window.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Run,
    Deploy,
    Details,
    Cancel,
}

/// Run an AppleScript with arguments, returning its stdout.
///
/// Arguments arrive in `argv` rather than inside the script text — see the
/// module docs for why that is not a style preference.
#[cfg(target_os = "macos")]
fn osascript(script: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("osascript")
        .arg("-")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write as _;
            child.stdin.take()?.write_all(script.as_bytes()).ok()?;
            child.wait_with_output().ok()
        })?;

    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(not(target_os = "macos"))]
fn osascript(_script: &str, _args: &[&str]) -> Option<String> {
    None
}

/// Show the package and ask what to do with it.
///
/// The action list is chosen from `kind`, which is what makes one extension
/// with two behaviours simpler than two extensions (§00.14, P-1): the branch
/// happens here, on data the manifest already carries.
pub fn choose_action(i: &Inspection) -> Action {
    if !cfg!(target_os = "macos") {
        // No window on this platform yet; running is the documented behaviour
        // of `cln run`, and the caller is a CLI invocation in that case.
        return Action::Run;
    }

    let body = summary(i);

    // Two dialogs rather than one: AppleScript's `display dialog` allows at
    // most three buttons, and a server package needs Cancel, Details, Run and
    // Deploy. `choose from list` has no such limit but reads poorly for two
    // options, so each kind gets the shape that fits it.
    let script = if i.is_server() {
        r#"on run argv
  set t to item 1 of argv
  set b to item 2 of argv
  set choices to {"Deploy to Clean Cloud", "Run locally", "Show details"}
  set picked to choose from list choices with title t with prompt b default items {"Deploy to Clean Cloud"} OK button name "Continue" cancel button name "Cancel"
  if picked is false then
    return "cancel"
  end if
  set p to item 1 of picked
  if p is "Deploy to Clean Cloud" then
    return "deploy"
  else if p is "Run locally" then
    return "run"
  else
    return "details"
  end if
end run"#
    } else {
        r#"on run argv
  set t to item 1 of argv
  set b to item 2 of argv
  display dialog b with title t buttons {"Cancel", "Show details", "Run"} default button "Run" with icon note
  set r to button returned of the result
  if r is "Run" then
    return "run"
  else if r is "Show details" then
    return "details"
  else
    return "cancel"
  end if
end run"#
    };

    match osascript(script, &[&i.name, &body]).as_deref() {
        Some("run") => Action::Run,
        Some("deploy") => Action::Deploy,
        Some("details") => Action::Details,
        // A cancelled dialog exits nonzero and prints nothing; treat anything
        // unrecognized as "do nothing", which is the safe reading.
        _ => Action::Cancel,
    }
}

/// The one-screen summary shown above the buttons.
fn summary(i: &Inspection) -> String {
    let mut s = String::new();
    if let Some(d) = &i.description {
        s.push_str(d);
        s.push_str("\n\n");
    }
    s.push_str(&format!("Version    {}\n", i.version));
    s.push_str(&format!("Type       {}\n", i.kind_label()));
    if let Some(v) = &i.runtime_resolved {
        s.push_str(&format!("Runtime    clean-runtime {v}\n"));
    }
    s.push_str(&format!(
        "Signature  {}",
        if i.signed { "present" } else { "unsigned" }
    ));
    s
}

/// The full field list, for the "Show details" action.
pub fn details_dialog(i: &Inspection) {
    let mut body = summary(i);
    if !i.worlds.is_empty() {
        body.push_str(&format!("\nWorlds     {}", i.worlds.join(", ")));
    }
    body.push_str(&format!("\nFile       {}", i.path.display()));
    info_dialog(&format!("{} {}", i.name, i.version), &body);
}

pub fn info_dialog(title: &str, body: &str) {
    let script = r#"on run argv
  display dialog (item 2 of argv) with title (item 1 of argv) buttons {"OK"} default button "OK" with icon note
end run"#;
    let _ = osascript(script, &[title, body]);
}

/// Report a failure where a double-click can actually see it.
///
/// stderr goes nowhere when Finder launched the process, so an error that only
/// printed would be indistinguishable from the silent do-nothing this whole
/// design exists to avoid.
pub fn error_dialog(title: &str, body: &str) {
    let script = r#"on run argv
  display dialog (item 2 of argv) with title (item 1 of argv) buttons {"OK"} default button "OK" with icon caution
end run"#;
    if osascript(script, &[title, body]).is_none() {
        eprintln!("{title}\n{body}");
    }
}

/// Run the package in a Terminal window.
///
/// A CLI guest's entire output contract is stdout, so a terminal is the honest
/// surface for it: capturing into a dialog would lose streaming, stdin, and
/// scrollback. P-3 forbids answering a double-click with a terminal for the
/// *server* case it was written about; this is the CLI exception, signed off
/// with that reasoning.
///
/// For a server package the terminal is also where the log goes, and the
/// window doubles as the stop control — closing it or pressing Ctrl-C ends the
/// server, which is otherwise a process with no visible owner.
pub fn run_in_terminal(i: &Inspection, layout: &Layout) -> anyhow::Result<ExitCode> {
    let cln = layout.bin_dir().join("cln");
    let cln = if cln.is_file() {
        cln
    } else {
        std::env::current_exe()?
    };

    // The script Terminal runs is written to a file, so neither the artifact
    // path nor the binary path is ever interpolated into AppleScript.
    let script = launch_script(&cln, &i.path, i.is_server());
    let dir = std::env::temp_dir();
    // Unique per call: two packages opened at once must not share a script,
    // and the script deletes itself on start.
    let path = dir.join(format!(
        "cln-open-{}-{}.sh",
        std::process::id(),
        next_temp_id()
    ));
    write_executable(&path, &script)?;

    let opened = std::process::Command::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(&path)
        .status();

    match opened {
        Ok(s) if s.success() => Ok(ExitCode::SUCCESS),
        _ => {
            error_dialog(
                "Could not open Terminal",
                &format!(
                    "Run it from a shell instead:\n\n    cln run {}",
                    i.path.display()
                ),
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

/// The per-run script Terminal executes.
///
/// The window is held open after the guest exits. Terminal's default is to
/// close on exit, which for a program that prints one line and returns is a
/// window that flashes and vanishes — the exact failure this design exists to
/// prevent.
fn launch_script(cln: &Path, artifact: &Path, is_server: bool) -> String {
    let cln = shell_quote(&cln.to_string_lossy());
    let artifact_q = shell_quote(&artifact.to_string_lossy());

    // A server prints its listening address and then blocks, so the browser is
    // opened once the port answers. A CLI guest runs to completion instead.
    let server_extra = if is_server {
        r#"
# Open the browser once the server answers, then get out of the way. The URL is
# read from the config the bundle carries rather than assumed, so a bundle
# listening somewhere else still opens the right page.
(
  url=""
  for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    listen=$(sed -n 's/^[[:space:]]*listen[[:space:]]*=[[:space:]]*"\(.*\)".*/\1/p' "$CFG" 2>/dev/null | head -1)
    if [ -n "$listen" ]; then
      url="http://$listen/"
      if curl -s -o /dev/null -m 1 "$url" 2>/dev/null; then
        open "$url"
        break
      fi
    fi
    sleep 1
  done
) &
"#
    } else {
        ""
    };

    let banner = if is_server {
        r#"echo "[Clean] Starting server. Press Ctrl-C to stop it."
echo"#
    } else {
        "clear"
    };

    format!(
        r#"#!/bin/sh
# GENERATED by `cln open`. Removed as soon as it starts.
rm -f "$0"

CLN={cln}
ARTIFACT={artifact_q}

# The extracted bundle's config, used only to find the listen address.
CFG=$(ls -d "$HOME/.cln/cache/run/"*/config/host.toml 2>/dev/null | head -1)
{server_extra}
{banner}
"$CLN" run "$ARTIFACT"
status=$?

echo
if [ $status -eq 0 ]; then
    echo "[Clean] finished."
else
    echo "[Clean] exited with status $status."
fi
echo "Press return to close this window."
read _ignored
exit $status
"#
    )
}

/// A counter making temp paths unique within one process.
fn next_temp_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Quote a string as a single shell word.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn write_executable(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::File::create(path)?;
    f.write_all(contents.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    f.sync_all()?;
    drop(f);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hostile_path_is_quoted_not_interpolated() {
        let s = launch_script(
            Path::new("/bin/cln"),
            Path::new("/tmp/it's; rm -rf ~/x.clapp"),
            false,
        );
        assert!(s.contains(r"'/tmp/it'\''s; rm -rf ~/x.clapp'"));
        // The dangerous fragment must never appear as a runnable command.
        assert!(!s.contains("\nrm -rf"));
    }

    /// The window must outlive a guest that prints one line and exits.
    #[test]
    fn the_window_is_held_open() {
        let s = launch_script(Path::new("/bin/cln"), Path::new("/tmp/a.clapp"), false);
        assert!(s.contains("read _ignored"));
        assert!(s.contains("exited with status"));
    }

    /// A server launch opens the browser; a CLI launch does not.
    #[test]
    fn only_a_server_launch_opens_a_browser() {
        let cli = launch_script(Path::new("/bin/cln"), Path::new("/tmp/a.clapp"), false);
        assert!(!cli.contains("open \"$url\""));

        let srv = launch_script(Path::new("/bin/cln"), Path::new("/tmp/a.clapp"), true);
        assert!(srv.contains("open \"$url\""));
        assert!(srv.contains("Ctrl-C"));
    }
}
