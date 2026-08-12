use serde::{Deserialize, Serialize};

use crate::{Platform, ToolchainKind};

/// One installable toolchain release, after we've resolved a GitHub Release
/// down to the single asset for the current platform.
///
/// Manager builds this by walking `GET /repos/<owner>/<repo>/releases` and
/// mapping the release-tag → version, plus the platform-matching asset →
/// download URL and SHA-256 (from the accompanying `.sha256` sidecar).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseEntry {
    pub kind: ToolchainKind,
    pub version: semver::Version,
    pub platform: Platform,
    pub asset_url: String,
    pub asset_sha256: String,
    /// Compat ranges for the *other two* toolchain kinds, per PLAN.md §7 Q2.
    /// Optional in M0 — we don't enforce it until dispatch exists in M1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<Compatibility>,
}

/// Compatibility windows a release advertises against the other kinds.
/// Empty ranges mean "no declared constraint" — different from missing.
#[derive(Clone, Debug, Eq, PartialEq, Default, Serialize, Deserialize)]
pub struct Compatibility {
    #[serde(default)]
    pub compiler: Vec<semver::VersionReq>,
    #[serde(default)]
    pub framework: Vec<semver::VersionReq>,
    #[serde(default)]
    pub runtime: Vec<semver::VersionReq>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Arch, Os};

    #[test]
    fn release_entry_json_roundtrip() {
        let entry = ReleaseEntry {
            kind: ToolchainKind::Compiler,
            version: "1.2.3".parse().unwrap(),
            platform: Platform {
                os: Os::Macos,
                arch: Arch::Arm64,
            },
            asset_url: "https://example.test/asset.tar.gz".into(),
            asset_sha256: "deadbeef".into(),
            compatibility: Some(Compatibility {
                framework: vec!["^2.1".parse().unwrap()],
                runtime: vec!["^1.0".parse().unwrap()],
                ..Default::default()
            }),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: ReleaseEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn compatibility_defaults_to_none_and_is_skipped() {
        let entry = ReleaseEntry {
            kind: ToolchainKind::Runtime,
            version: "0.1.0".parse().unwrap(),
            platform: Platform {
                os: Os::Linux,
                arch: Arch::X86_64,
            },
            asset_url: "u".into(),
            asset_sha256: "s".into(),
            compatibility: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(
            !json.contains("compatibility"),
            "None compatibility should be skipped in output, got: {json}"
        );
    }
}
