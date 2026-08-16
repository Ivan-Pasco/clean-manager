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
//! # Why an AppleScript droplet, and not a shell script
//!
//! **macOS does not pass the double-clicked file as `argv`.** A bundle launched
//! from Finder receives its documents as an `odoc` Apple Event, delivered to the
//! running application — the executable starts with *no arguments at all*. A
//! shell script as `CFBundleExecutable` therefore launches, sees `$#` of zero,
//! and exits without ever learning which file the user opened. That is not a
//! theoretical concern: this bundle was first written that way, and the failure
//! is silent — Finder reports nothing, and the only symptom is that
//! double-clicking appears to do nothing at all.
//!
//! Receiving an Apple Event requires an event loop and an `aevt`/`odoc`
//! handler. AppleScript's `on open` is exactly that, and `osacompile` produces a
//! bundle that has one without needing a compiler or a signing identity. So the
//! executable is a compiled AppleScript droplet whose whole body forwards the
//! dropped paths to a shell command.
//!
//! `osacompile` writes its own `Info.plist`, so manager compiles first and then
//! rewrites that file with the document-type declarations — the order matters,
//! since doing it the other way loses them.
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
    <!-- osacompile names the executable `droplet` when the script has an
         `on open` handler (`applet` when it does not). Naming anything else
         here leaves a bundle Finder cannot launch. -->
    <key>CFBundleExecutable</key>
    <string>droplet</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.13</string>
    <!-- Not LSBackgroundOnly: the droplet shows dialogs, and a background-only
         app cannot bring a window to the front. -->
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

/// The AppleScript the droplet runs when a document is opened.
///
/// Its whole body is a handoff: `cln open <path>` owns every decision about
/// what the package is and what to offer. Keeping the logic in the binary
/// rather than in the script means the window cannot drift from the CLI, and
/// that fixing behaviour ships with `cln` rather than requiring the bundle to
/// be rewritten.
///
/// `on open` is the Apple Event handler — the reason this is AppleScript at
/// all. `on run` covers the case of launching the app with no document, which
/// Finder does if someone opens it from Applications directly.
fn droplet_source(cln: &Path) -> String {
    let cln = applescript_string(&cln.to_string_lossy());
    format!(
        r#"-- GENERATED by `cln register`. Do not edit; rewritten on every install.
--
-- Receives a double-clicked Clean package and hands it to `cln open`, which
-- shows what the package is and offers the actions valid for its kind.

on open theFiles
  repeat with f in theFiles
    set p to POSIX path of f
    try
      do shell script {cln} & " open " & quoted form of p
    on error errMsg
      -- `cln open` reports its own failures in a dialog; this covers the case
      -- where the binary is missing entirely, which it cannot report itself.
      display dialog "Could not open this Clean package." & return & return & errMsg ¬
        with title "Clean" buttons {{"OK"}} default button "OK" with icon caution
    end try
  end repeat
end open

on run
  display dialog "Open a .clapp file to run or deploy it." ¬
    with title "Clean" buttons {{"OK"}} default button "OK" with icon note
end run
"#,
        cln = cln
    )
}

/// Quote a string as an AppleScript literal.
///
/// Only quotes and backslashes need escaping; a path containing either would
/// otherwise terminate the literal and change the meaning of the script.
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

/// Build the `.app` bundle, replacing any bundle already there.
///
/// Two steps, in this order:
///
/// 1. `osacompile` compiles the droplet, producing a bundle with an Apple
///    Event handler and its own `Info.plist`.
/// 2. That `Info.plist` is replaced with one that also declares the document
///    types, since `osacompile` writes no `CFBundleDocumentTypes`.
///
/// The order matters: writing the plist first and compiling second discards
/// it, which is a silent failure — the bundle works but Finder never binds
/// `.clapp` to it.
///
/// Replacing rather than merging is what makes this idempotent: the bundle is
/// fully generated, so the result depends only on the current `cln` path and
/// version, never on what an older manager left behind.
pub fn write_bundle(home: &Path, cln: &Path, version: &str) -> std::io::Result<PathBuf> {
    let bundle = bundle_path(home);

    if bundle.exists() {
        std::fs::remove_dir_all(&bundle)?;
    }
    if let Some(parent) = bundle.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let source = droplet_source(cln);
    compile_droplet(&bundle, &source)?;

    // Must come after compiling: osacompile overwrites this file.
    std::fs::write(
        bundle.join("Contents").join("Info.plist"),
        info_plist(version),
    )?;

    Ok(bundle)
}

/// A counter making temp paths unique within one process.
fn next_temp_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Run `osacompile` over the droplet source.
///
/// The source is written to a file rather than piped, because `osacompile`
/// reads its input as a path and gives a clearer error when the file is bad.
fn compile_droplet(bundle: &Path, source: &str) -> std::io::Result<()> {
    // Keyed on a per-call counter as well as the pid: two registrations in one
    // process (the test suite does exactly this, in parallel) would otherwise
    // pick the same path, and one would delete the other's source mid-compile.
    let scpt = std::env::temp_dir().join(format!(
        "cln-droplet-{}-{}.applescript",
        std::process::id(),
        next_temp_id()
    ));
    std::fs::write(&scpt, source)?;

    let out = std::process::Command::new("/usr/bin/osacompile")
        .arg("-o")
        .arg(bundle)
        .arg(&scpt)
        .output()?;

    let _ = std::fs::remove_file(&scpt);

    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "osacompile failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
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
        assert!(bundle.join("Contents/MacOS/droplet").is_file());
        assert!(bundle.ends_with("Applications/Clean.app"));
    }

    /// `osacompile` names the executable `applet`; a plist naming anything
    /// else produces a bundle Finder cannot launch.
    #[test]
    fn the_plist_names_the_executable_osacompile_produced() {
        let home = tempdir().unwrap();
        let bundle =
            write_bundle(home.path(), Path::new("/Users/a/.cln/bin/cln"), "0.1.9").unwrap();

        let exe = bundle.join("Contents/MacOS/droplet");
        assert!(
            exe.is_file(),
            "a script with `on open` compiles to Contents/MacOS/droplet"
        );

        let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains("<string>droplet</string>"));
    }

    /// The document types must survive compilation: osacompile writes its own
    /// Info.plist, so writing ours first would silently discard them.
    #[test]
    fn the_document_types_survive_compilation() {
        let home = tempdir().unwrap();
        let bundle =
            write_bundle(home.path(), Path::new("/Users/a/.cln/bin/cln"), "0.1.9").unwrap();

        let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains("CFBundleDocumentTypes"));
        assert!(plist.contains("dev.cleanlanguage.clapp"));
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

    /// The bug this whole design exists to prevent: macOS delivers a
    /// double-clicked document as an Apple Event, never as `argv`. A handler
    /// without an `on open` block launches, receives nothing, and exits
    /// silently — which is indistinguishable from the association not working.
    #[test]
    fn the_droplet_handles_the_open_event() {
        let src = droplet_source(Path::new("/Users/a/.cln/bin/cln"));
        assert!(src.contains("on open theFiles"), "must handle `odoc`");
        assert!(src.contains("POSIX path of f"));
        assert!(src.contains(" open "), "must hand the path to `cln open`");
    }

    /// The droplet delegates rather than deciding: every behaviour lives in
    /// `cln open`, so the window cannot drift from the CLI.
    #[test]
    fn the_droplet_only_delegates() {
        let src = droplet_source(Path::new("/Users/a/.cln/bin/cln"));
        assert!(src.contains("/Users/a/.cln/bin/cln"));
        // No decisions about kind, runtime, or which actions to offer belong
        // in the script — those live in `cln open`. (`.clapp` appears only in
        // the bare-launch help text.)
        assert!(!src.contains("Terminal"));
        assert!(!src.contains("Deploy"));
        assert!(!src.contains("Run locally"));
        assert!(!src.contains("install runtime"));
    }

    /// A path with a quote must not terminate the AppleScript literal.
    #[test]
    fn a_hostile_binary_path_is_escaped_for_applescript() {
        let src = droplet_source(Path::new("/tmp/it\"s/cln"));
        assert!(src.contains(r#"/tmp/it\"s/cln"#));
    }

    #[test]
    fn quoted_form_is_used_for_the_document_path() {
        let src = droplet_source(Path::new("/a/cln"));
        assert!(
            src.contains("quoted form of p"),
            "the document path must reach the shell quoted"
        );
    }

    /// Launching the app with no document — opening it from Applications
    /// directly — must say what it is for rather than doing nothing.
    ///
    /// The previous implementation treated "no arguments" as the normal case
    /// and exited silently, which is precisely why the association appeared
    /// broken: every Finder launch takes that path.
    #[test]
    fn launching_with_no_document_explains_itself() {
        let src = droplet_source(Path::new("/a/cln"));
        assert!(src.contains("on run"), "must handle a bare launch");
        assert!(src.contains("Open a .clapp file"));
    }
}
