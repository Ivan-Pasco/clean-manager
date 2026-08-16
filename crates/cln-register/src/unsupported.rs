//! Platforms where registration is not implemented yet.
//!
//! Windows and Linux registration is the M3 deliverable: §00.12 specifies both
//! (`HKCU\Software\Classes` keys, and an `xdg-mime` `.desktop` handler), and
//! PLAN.md §5 requires a per-OS test matrix before either can be trusted. That
//! matrix does not exist yet, and neither does the CI coverage to keep it
//! honest.
//!
//! **This module exists so those platforms fail loudly.** A registration stub
//! that silently did nothing would be worse than no stub at all: `cln install`
//! would report success, the user would double-click a `.clapp`, nothing would
//! happen, and there would be no message anywhere connecting that to an
//! unimplemented feature. Every entry point here returns an error naming the
//! platform, and says what to do instead in the meantime.

use crate::state::Extension;

/// The name of the platform this binary was built for, as a user would say it.
pub fn platform_label() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

/// The message shown when registration is attempted on an unsupported OS.
///
/// It names the platform, states plainly that the feature is not built yet
/// rather than implying a failure, and gives the command that does work today
/// so the user is not simply stopped.
pub fn message() -> String {
    format!(
        "file associations are not implemented on {platform} yet\n\
         \n\
         Double-clicking a {exts} file will not run it on this platform. \
         macOS registration ships today; {platform} support is planned and \
         tracked as part of Manager §00.12.\n\
         \n\
         In the meantime, run artifacts from a terminal:\n\
         \n    cln run <file>.clapp",
        platform = platform_label(),
        exts = Extension::ALL
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(" or "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure must name the platform and point somewhere useful. A stub
    /// that said only "unsupported" would leave the user with no next step.
    #[test]
    fn the_message_names_the_platform_and_a_working_command() {
        let m = message();
        assert!(m.contains(platform_label()));
        assert!(m.contains("cln run"));
        assert!(m.contains("not implemented"));
    }

    /// One compiled extension, per §00.14 P-1.
    #[test]
    fn the_message_names_the_package_extension() {
        let m = message();
        assert!(m.contains(".clapp"));
        assert!(!m.contains(".serve"), "`.serve` was retired");
    }
}
