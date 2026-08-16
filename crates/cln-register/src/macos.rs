//! macOS file associations — a Launch Services `.app` bundle under
//! `~/Applications/`, per §00.12's macOS row.
//!
//! # Why an `.app` bundle at all
//!
//! Launch Services binds a document type to an *application*, not to a command
//! line. There is no per-user API that says "open `.clapp` with this binary and
//! these arguments" — the association is always to a bundle identifier, and the
//! bundle declares which types it handles via `CFBundleDocumentTypes` in its
//! `Info.plist`. So a bundle is the mechanism, not a workaround.
//!
//! The bundle manager writes is a real one, kept deliberately minimal: a
//! `Info.plist` declaring the types, and a shell script as the executable. It
//! is not a compiled application and holds no logic beyond forwarding its
//! arguments to `cln run`.
//!
//! # Why a Terminal window
//!
//! A double-clicked bundle has **no controlling terminal**. Its stdout goes
//! nowhere a user can see: `hello` would be written to a void, and the process
//! would exit 0 with no visible effect at all. Since a `.clapp` today targets
//! the `cli` world — a program whose entire output contract is stdout — the
//! launcher asks `Terminal.app` to open a per-run script instead of executing
//! the guest directly.
//!
//! The alternative considered was capturing stdout and showing it in a dialog.
//! It was rejected: it truncates long output, it cannot support a guest that
//! reads stdin, and it misrepresents a CLI program as a GUI one. A terminal
//! window is what a CLI artifact honestly is, and it gives the user scrollback
//! and a visible exit status for free.
//!
//! When a GUI-shaped world exists, the launcher gains a branch on the
//! manifest's world. That branch is deliberately absent today rather than
//! written blind against a world nothing can produce.

use std::path::{Path, PathBuf};

use crate::state::Extension;

/// Where the bundle lives. `~/Applications/` is the per-user applications
/// directory: no elevated privileges, and it does not collide with a
/// system-wide install in `/Applications/`.
pub fn bundle_path(home: &Path) -> PathBuf {
    home.join("Applications").join("Clean.app")
}

/// The bundle identifier Launch Services keys the association on.
pub const BUNDLE_ID: &str = "dev.cleanlanguage.cln";

/// `Info.plist` — declares the bundle, and every type it opens.
///
/// Two declarations per extension are needed and they do different jobs:
/// `UTExportedTypeDeclarations` *defines* the type (this is what `.clapp`
/// means), and `CFBundleDocumentTypes` *claims* it (this app opens it).
/// Exporting without claiming registers a filetype nothing opens; claiming
/// without exporting binds to a UTI no one defined.
fn info_plist(version: &str) -> String {
    let mut exported = String::new();
    let mut documents = String::new();

    for ext in Extension::ALL {
        exported.push_str(&format!(
            r#"
        <dict>
            <key>UTTypeIdentifier</key>
            <string>{uti}</string>
            <key>UTTypeDescription</key>
            <string>{desc}</string>
            <key>UTTypeConformsTo</key>
            <array>
                <string>public.data</string>
                <string>public.archive</string>
            </array>
            <key>UTTypeTagSpecification</key>
            <dict>
                <key>public.filename-extension</key>
                <array>
                    <string>{ext}</string>
                </array>
            </dict>
        </dict>"#,
            uti = ext.uti(),
            desc = ext.description(),
            ext = ext.as_str(),
        ));

        // `LSHandlerRank: Owner` states this app owns the type rather than
        // merely being able to open it — the correct rank for a format the
        // project defines, and what makes it the default handler.
        documents.push_str(&format!(
            r#"
        <dict>
            <key>CFBundleTypeName</key>
            <string>{desc}</string>
            <key>CFBundleTypeRole</key>
            <string>Viewer</string>
            <key>LSHandlerRank</key>
            <string>Owner</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>{uti}</string>
            </array>
            <key>CFBundleTypeExtensions</key>
            <array>
                <string>{ext}</string>
            </array>
        </dict>"#,
            desc = ext.description(),
            uti = ext.uti(),
            ext = ext.as_str(),
        ));
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Clean</string>
    <key>CFBundleDisplayName</key>
    <string>Clean</string>
    <key>CFBundleIdentifier</key>
    <string>{BUNDLE_ID}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>cln-open</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.13</string>
    <!-- No Dock icon: this bundle exists to route a document to a terminal,
         not to be launched on its own. -->
    <key>LSBackgroundOnly</key>
    <true/>
    <key>UTExportedTypeDeclarations</key>
    <array>{exported}
    </array>
    <key>CFBundleDocumentTypes</key>
    <array>{documents}
    </array>
</dict>
</plist>
"#
    )
}

/// The bundle's executable: receives the double-clicked path, shows the run in
/// a Terminal window.
///
/// Two things here are load-bearing and easy to get wrong.
///
/// **The path is passed through a file, not interpolated into AppleScript.**
/// Building a `do script "cln run <path>"` string would break on any path
/// containing a quote or backslash, and would execute whatever a crafted
/// filename contained. The launcher instead writes a per-run script with the
/// path quoted by the shell, and tells Terminal to run *that*.
///
/// **The window is told to stay open.** Terminal's default is to close on
/// exit, which for a program that prints one line and returns is a window that
/// flashes and vanishes — the exact failure this design exists to avoid. The
/// trailing read holds it until the user dismisses it, and the exit status is
/// printed so a failing guest is visible rather than silent.
fn launcher_script(cln: &Path) -> String {
    format!(
        r#"#!/bin/sh
# GENERATED by `cln register`. Do not edit -- rewritten on every install.
#
# Opens a double-clicked Clean artifact in a Terminal window. See
# cln-register::macos for why this indirection exists.

set -u

CLN={cln}

# Launch Services passes the document path as $1. With no argument there is
# nothing to run: the bundle is a document handler, not a launchable app.
if [ "$#" -eq 0 ]; then
    exit 0
fi

for target in "$@"; do
    run_script=$(mktemp /tmp/cln-open.XXXXXX) || exit 1

    # The artifact path is expanded *now*, into a single-quoted shell word, so
    # the generated script contains no unquoted user input. A literal quote in
    # the filename is escaped by closing, escaping, and reopening the quote.
    quoted=$(printf "%s" "$target" | sed "s/'/'\\\\''/g")
    quoted_cln=$(printf "%s" "$CLN" | sed "s/'/'\\\\''/g")

    cat > "$run_script" <<SCRIPT
#!/bin/sh
rm -f '$run_script'
clear
'$quoted_cln' run '$quoted'
status=\$?
echo
if [ \$status -eq 0 ]; then
    echo "[Clean] finished."
else
    echo "[Clean] exited with status \$status."
fi
echo "Press return to close this window."
read _ignored
exit \$status
SCRIPT

    chmod +x "$run_script"

    # `open -a Terminal <script>` runs the script in a new window and returns.
    # Preferred over `osascript` because no user data is interpolated into an
    # AppleScript string.
    open -a Terminal "$run_script"
done
"#,
        cln = shell_single_quote(&cln.to_string_lossy()),
    )
}

/// Quote a string as a single shell word.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Write the `.app` bundle to disk, replacing any bundle already there.
///
/// Replacing rather than merging is what makes this idempotent: the bundle is
/// fully generated, so the state after writing depends only on the current
/// `cln` path and version, never on what a previous version left behind.
pub fn write_bundle(home: &Path, cln: &Path, version: &str) -> std::io::Result<PathBuf> {
    let bundle = bundle_path(home);
    let macos_dir = bundle.join("Contents").join("MacOS");

    // A stale bundle from an older layout would otherwise keep files that are
    // no longer written.
    if bundle.exists() {
        std::fs::remove_dir_all(&bundle)?;
    }
    std::fs::create_dir_all(&macos_dir)?;

    std::fs::write(
        bundle.join("Contents").join("Info.plist"),
        info_plist(version),
    )?;

    let exe = macos_dir.join("cln-open");
    write_executable(&exe, &launcher_script(cln))?;

    // Bundles are cached aggressively by Launch Services, keyed partly on
    // mtime. Touching the bundle root after writing makes a re-register of an
    // unchanged path still read as new.
    let _ = std::fs::File::open(&bundle);

    Ok(bundle)
}

/// Write a file and make it executable, closing the handle before the mode is
/// observed by anything else.
fn write_executable(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let mut f = std::fs::File::create(path)?;
    f.write_all(contents.as_bytes())?;
    f.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    f.sync_all()?;
    drop(f);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn the_bundle_has_the_structure_launch_services_requires() {
        let home = tempdir().unwrap();
        let bundle =
            write_bundle(home.path(), Path::new("/Users/a/.cln/bin/cln"), "0.1.9").unwrap();

        assert!(bundle.join("Contents/Info.plist").is_file());
        assert!(bundle.join("Contents/MacOS/cln-open").is_file());
        assert!(bundle.ends_with("Applications/Clean.app"));
    }

    #[test]
    fn the_launcher_is_executable() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempdir().unwrap();
        let bundle =
            write_bundle(home.path(), Path::new("/Users/a/.cln/bin/cln"), "0.1.9").unwrap();

        let mode = std::fs::metadata(bundle.join("Contents/MacOS/cln-open"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "launcher must be executable");
    }

    #[test]
    fn the_plist_declares_and_claims_every_extension() {
        let plist = info_plist("0.1.9");
        for ext in Extension::ALL {
            assert!(plist.contains(ext.uti()), "{ext} UTI missing");
            assert!(
                plist.matches(ext.as_str()).count() >= 2,
                "{ext} must be both exported and claimed"
            );
        }
        assert!(plist.contains("LSHandlerRank"));
        assert!(plist.contains(BUNDLE_ID));
    }

    /// §00.12: `.wasm` MUST NOT be claimed under any circumstance.
    #[test]
    fn the_plist_never_claims_wasm() {
        let plist = info_plist("0.1.9");
        assert!(!plist.contains("wasm"), "the bundle must never claim .wasm");
        assert!(!plist.contains("public.wasm"));
    }

    /// The whole point of the Terminal indirection: the window must outlive
    /// a program that prints one line and exits.
    #[test]
    fn the_launcher_holds_the_window_open_and_reports_status() {
        let script = launcher_script(Path::new("/Users/a/.cln/bin/cln"));
        assert!(script.contains("read _ignored"), "window must not vanish");
        assert!(script.contains("exited with status"));
        assert!(script.contains("open -a Terminal"));
    }

    /// A path with a quote in it must not break the generated script or run
    /// anything the filename contains.
    #[test]
    fn a_hostile_binary_path_is_quoted_not_interpolated() {
        let script = launcher_script(Path::new("/tmp/it's/cln"));
        assert!(script.contains(r"'/tmp/it'\''s/cln'"));
        assert!(!script.contains("$(rm"));
    }

    /// Re-registering must converge on the same bundle, not accumulate.
    #[test]
    fn writing_twice_produces_identical_bundles() {
        let home = tempdir().unwrap();
        let cln = Path::new("/Users/a/.cln/bin/cln");

        let b1 = write_bundle(home.path(), cln, "0.1.9").unwrap();
        let plist1 = std::fs::read_to_string(b1.join("Contents/Info.plist")).unwrap();
        let exe1 = std::fs::read_to_string(b1.join("Contents/MacOS/cln-open")).unwrap();

        let b2 = write_bundle(home.path(), cln, "0.1.9").unwrap();
        let plist2 = std::fs::read_to_string(b2.join("Contents/Info.plist")).unwrap();
        let exe2 = std::fs::read_to_string(b2.join("Contents/MacOS/cln-open")).unwrap();

        assert_eq!(b1, b2);
        assert_eq!(plist1, plist2);
        assert_eq!(exe1, exe2);
    }

    /// An upgrade rebinds to the new binary rather than leaving the old path.
    #[test]
    fn rewriting_rebinds_to_the_new_binary() {
        let home = tempdir().unwrap();
        write_bundle(home.path(), Path::new("/old/cln"), "0.1.8").unwrap();
        let bundle = write_bundle(home.path(), Path::new("/new/cln"), "0.1.9").unwrap();

        let exe = std::fs::read_to_string(bundle.join("Contents/MacOS/cln-open")).unwrap();
        assert!(exe.contains("/new/cln"));
        assert!(!exe.contains("/old/cln"));
    }

    /// Stale files from a previous layout must not survive a re-register.
    #[test]
    fn a_stale_file_in_the_old_bundle_does_not_survive() {
        let home = tempdir().unwrap();
        let bundle = write_bundle(home.path(), Path::new("/a/cln"), "0.1.8").unwrap();
        let stale = bundle.join("Contents/MacOS/leftover");
        std::fs::write(&stale, "old").unwrap();

        write_bundle(home.path(), Path::new("/a/cln"), "0.1.9").unwrap();
        assert!(!stale.exists(), "a full rewrite must clear stale files");
    }

    #[test]
    fn a_bundle_with_no_document_argument_does_nothing() {
        let script = launcher_script(Path::new("/a/cln"));
        assert!(script.contains(r#"if [ "$#" -eq 0 ]"#));
    }
}
