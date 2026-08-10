use std::fmt;

use serde::{Deserialize, Serialize};

/// The (os, arch) pair we match release assets against.
///
/// Manager fetches the GitHub Releases list for a component, then picks the
/// asset whose filename encodes this platform. The canonical filename shape is:
/// `<binary>-<version>-<os>-<arch>.<ext>` — see `Platform::asset_matches`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Macos,
    Linux,
    Windows,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    Arm64,
    X86_64,
}

impl Platform {
    /// Detect the host platform at runtime. Falls back to `None` on anything
    /// we don't ship for; the caller decides whether that's fatal.
    pub fn detect() -> Option<Platform> {
        let os = match std::env::consts::OS {
            "macos" => Os::Macos,
            "linux" => Os::Linux,
            "windows" => Os::Windows,
            _ => return None,
        };
        let arch = match std::env::consts::ARCH {
            "aarch64" | "arm64" => Arch::Arm64,
            "x86_64" => Arch::X86_64,
            _ => return None,
        };
        Some(Platform { os, arch })
    }

    /// The archive extension we ship on this OS. Unix uses tar.gz; Windows uses
    /// zip — per the release-artifact decision recorded in PLAN.md §7 Q3.
    pub fn archive_ext(self) -> &'static str {
        match self.os {
            Os::Windows => "zip",
            Os::Macos | Os::Linux => "tar.gz",
        }
    }

    /// True when `asset_name` matches this platform under the canonical
    /// filename shape `<anything>-<os>-<arch>.<ext>`. Substring match — we
    /// don't insist on a prefix so a repo may name its assets freely.
    pub fn asset_matches(self, asset_name: &str) -> bool {
        let needle = format!("-{}-{}.{}", self.os, self.arch, self.archive_ext());
        asset_name.ends_with(&needle)
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Os::Macos => "macos",
            Os::Linux => "linux",
            Os::Windows => "windows",
        })
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Arch::Arm64 => "arm64",
            Arch::X86_64 => "x86_64",
        })
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.os, self.arch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_matches_canonical_shape() {
        let p = Platform { os: Os::Macos, arch: Arch::Arm64 };
        assert!(p.asset_matches("clean-compiler-1.2.3-macos-arm64.tar.gz"));
        assert!(!p.asset_matches("clean-compiler-1.2.3-macos-x86_64.tar.gz"));
        assert!(!p.asset_matches("clean-compiler-1.2.3-linux-arm64.tar.gz"));
    }

    #[test]
    fn windows_uses_zip() {
        let p = Platform { os: Os::Windows, arch: Arch::X86_64 };
        assert_eq!(p.archive_ext(), "zip");
        assert!(p.asset_matches("clean-runtime-2.0.0-windows-x86_64.zip"));
        assert!(!p.asset_matches("clean-runtime-2.0.0-windows-x86_64.tar.gz"));
    }
}
