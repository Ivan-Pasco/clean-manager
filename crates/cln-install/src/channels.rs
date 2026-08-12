//! Release sources — where manager gets the list of available versions and
//! their download URLs / checksums.
//!
//! Two implementations:
//! - [`GithubReleases`] — the real source. Lists `/repos/<owner>/<repo>/releases`
//!   via the GitHub API, maps release tags to `semver::Version`, and finds the
//!   asset for the current platform plus its `.sha256` sidecar.
//! - [`LocalDir`] — a filesystem-backed source used by every Layer A test. A
//!   directory whose children are `<tag>/<asset>` and `<tag>/<asset>.sha256`.

use std::fs;
use std::path::{Path, PathBuf};

use cln_shared::{Platform, ReleaseEntry, ToolchainKind};
use semver::Version;
use serde::Deserialize;

use crate::repos::{resolve_repo, RepoRef};

/// A version selector — either an exact release or "latest stable".
#[derive(Clone, Debug)]
pub enum VersionSpec {
    Exact(Version),
    Latest,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("no release matching {spec:?} for {kind} on {platform}")]
    NoMatch {
        kind: ToolchainKind,
        platform: Platform,
        spec: String,
    },
    #[error("no asset for platform {platform} in release {tag} of {repo}")]
    NoAsset {
        repo: String,
        tag: String,
        platform: Platform,
    },
    #[error("missing SHA-256 sidecar for asset {asset} in release {tag}")]
    MissingChecksum { asset: String, tag: String },
    #[error("io error while listing releases: {0}")]
    Io(#[from] std::io::Error),
    #[error("network error while contacting release source: {0}")]
    Network(String),
    #[error("release source returned malformed data: {0}")]
    Malformed(String),
}

pub trait ReleaseSource {
    fn kind(&self) -> ToolchainKind;

    /// Every release this source knows about, resolved for the given platform.
    /// Ordering is unspecified — callers that want "latest" call `resolve` with
    /// [`VersionSpec::Latest`] which picks the max version.
    fn list(&self, platform: Platform) -> Result<Vec<ReleaseEntry>, ChannelError>;

    /// Resolve a `VersionSpec` to a single `ReleaseEntry` for `platform`.
    ///
    /// Default impl: list everything then pick. Concrete sources can override
    /// for efficiency (e.g. hit the `/releases/latest` endpoint).
    fn resolve(
        &self,
        spec: &VersionSpec,
        platform: Platform,
    ) -> Result<ReleaseEntry, ChannelError> {
        let mut all = self.list(platform)?;
        match spec {
            VersionSpec::Exact(want) => {
                all.into_iter()
                    .find(|e| &e.version == want)
                    .ok_or(ChannelError::NoMatch {
                        kind: self.kind(),
                        platform,
                        spec: want.to_string(),
                    })
            }
            VersionSpec::Latest => {
                all.retain(|e| e.version.pre.is_empty());
                all.sort_by(|a, b| a.version.cmp(&b.version));
                all.pop().ok_or(ChannelError::NoMatch {
                    kind: self.kind(),
                    platform,
                    spec: "latest".into(),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// LocalDir — filesystem-backed source for tests.
// ---------------------------------------------------------------------------

/// Serve releases from a local directory. Layout:
///
/// ```text
/// <root>/
///   1.0.0/
///     clean-compiler-1.0.0-macos-arm64.tar.gz
///     clean-compiler-1.0.0-macos-arm64.tar.gz.sha256
///   1.1.0/
///     clean-compiler-1.1.0-macos-arm64.tar.gz
///     clean-compiler-1.1.0-macos-arm64.tar.gz.sha256
/// ```
///
/// The directory name is the semver tag (no `v` prefix); asset filenames match
/// [`Platform::asset_matches`]. Sidecar files contain a hex SHA-256 as the
/// first whitespace-delimited token.
pub struct LocalDir {
    kind: ToolchainKind,
    root: PathBuf,
}

impl LocalDir {
    pub fn new(kind: ToolchainKind, root: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            root: root.into(),
        }
    }
}

impl ReleaseSource for LocalDir {
    fn kind(&self) -> ToolchainKind {
        self.kind
    }

    fn list(&self, platform: Platform) -> Result<Vec<ReleaseEntry>, ChannelError> {
        let mut out = Vec::new();
        let read = match fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for entry in read {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let tag = entry.file_name();
            let Some(tag) = tag.to_str() else { continue };
            let Ok(version) = Version::parse(tag) else {
                continue;
            };

            match resolve_local_asset(&entry.path(), platform, tag)? {
                Some((asset, sha)) => out.push(ReleaseEntry {
                    kind: self.kind,
                    version,
                    platform,
                    asset_url: format!("file://{}", asset.display()),
                    asset_sha256: sha,
                    compatibility: None,
                }),
                None => continue,
            }
        }
        Ok(out)
    }
}

fn resolve_local_asset(
    tag_dir: &Path,
    platform: Platform,
    tag: &str,
) -> Result<Option<(PathBuf, String)>, ChannelError> {
    let mut asset: Option<PathBuf> = None;
    for f in fs::read_dir(tag_dir)? {
        let f = f?;
        let name = f.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.ends_with(".sha256") {
            continue;
        }
        if platform.asset_matches(name) {
            asset = Some(f.path());
            break;
        }
    }
    let Some(asset) = asset else { return Ok(None) };

    let sha_path = {
        let mut p = asset.clone();
        let file = p.file_name().unwrap().to_owned();
        let mut sha_name = file;
        sha_name.push(".sha256");
        p.set_file_name(sha_name);
        p
    };
    let sha_text = fs::read_to_string(&sha_path).map_err(|_| ChannelError::MissingChecksum {
        asset: asset.file_name().unwrap().to_string_lossy().into(),
        tag: tag.into(),
    })?;
    let sha = sha_text
        .split_whitespace()
        .next()
        .ok_or_else(|| ChannelError::MissingChecksum {
            asset: asset.file_name().unwrap().to_string_lossy().into(),
            tag: tag.into(),
        })?
        .to_ascii_lowercase();
    Ok(Some((asset, sha)))
}

// ---------------------------------------------------------------------------
// GithubReleases — the production source.
// ---------------------------------------------------------------------------

/// Source that hits `api.github.com/repos/<owner>/<repo>/releases`.
///
/// Rate-limit handling: if `GITHUB_TOKEN` is set in the environment, it's
/// sent as a bearer token. Otherwise we fall back to unauthenticated calls
/// (60/hour per IP).
pub struct GithubReleases {
    kind: ToolchainKind,
    repo: RepoRef,
    api_base: String,
}

impl GithubReleases {
    pub fn new(kind: ToolchainKind) -> Self {
        Self {
            kind,
            repo: resolve_repo(kind),
            api_base: "https://api.github.com".into(),
        }
    }

    /// Override the API base — used to point at a mock server in tests.
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into();
        self
    }
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

impl ReleaseSource for GithubReleases {
    fn kind(&self) -> ToolchainKind {
        self.kind
    }

    fn list(&self, platform: Platform) -> Result<Vec<ReleaseEntry>, ChannelError> {
        let url = format!(
            "{}/repos/{}/{}/releases?per_page=100",
            self.api_base, self.repo.owner, self.repo.name
        );
        let mut req = ureq::get(&url)
            .set("Accept", "application/vnd.github+json")
            .set("User-Agent", "cln-manager");
        if let Ok(tok) = std::env::var("GITHUB_TOKEN") {
            if !tok.is_empty() {
                req = req.set("Authorization", &format!("Bearer {tok}"));
            }
        }
        let releases: Vec<GhRelease> = req
            .call()
            .map_err(|e| ChannelError::Network(e.to_string()))?
            .into_json()
            .map_err(|e| ChannelError::Malformed(e.to_string()))?;

        let mut out = Vec::new();
        for rel in releases {
            if rel.draft {
                continue;
            }
            let Some(version) = parse_tag(&rel.tag_name) else {
                continue;
            };
            let Some(asset) = rel.assets.iter().find(|a| platform.asset_matches(&a.name)) else {
                continue;
            };
            let sha_name = format!("{}.sha256", asset.name);
            let Some(sha_asset) = rel.assets.iter().find(|a| a.name == sha_name) else {
                continue;
            };
            let sha_text = ureq::get(&sha_asset.browser_download_url)
                .set("User-Agent", "cln-manager")
                .call()
                .map_err(|e| ChannelError::Network(e.to_string()))?
                .into_string()
                .map_err(|e| ChannelError::Malformed(e.to_string()))?;
            let sha = sha_text
                .split_whitespace()
                .next()
                .ok_or_else(|| ChannelError::MissingChecksum {
                    asset: asset.name.clone(),
                    tag: rel.tag_name.clone(),
                })?
                .to_ascii_lowercase();

            let mut entry = ReleaseEntry {
                kind: self.kind,
                version,
                platform,
                asset_url: asset.browser_download_url.clone(),
                asset_sha256: sha,
                compatibility: None,
            };
            // Prerelease flag is informational — we still surface these; the
            // resolver's `Latest` path filters them out via `pre.is_empty()`.
            if rel.prerelease {
                entry.compatibility = None;
            }
            out.push(entry);
        }
        Ok(out)
    }
}

/// Strip an optional `v` prefix from a GitHub tag and parse the rest as semver.
fn parse_tag(tag: &str) -> Option<Version> {
    let stripped = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(stripped).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cln_shared::platform::{Arch, Os};
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    fn plat() -> Platform {
        Platform {
            os: Os::Macos,
            arch: Arch::Arm64,
        }
    }

    fn seed_release(root: &Path, tag: &str, kind: ToolchainKind, platform: Platform) {
        let dir = root.join(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let name = format!(
            "{}-{}-{}.{}",
            kind.binary_name(),
            tag,
            platform,
            platform.archive_ext()
        );
        let bytes = format!("payload for {tag}").into_bytes();
        std::fs::write(dir.join(&name), &bytes).unwrap();
        let sha = hex(Sha256::digest(&bytes).as_slice());
        std::fs::write(
            dir.join(format!("{name}.sha256")),
            format!("{sha}  {name}\n"),
        )
        .unwrap();
    }

    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    #[test]
    fn local_dir_lists_matching_releases() {
        let tmp = tempdir().unwrap();
        seed_release(tmp.path(), "1.0.0", ToolchainKind::Compiler, plat());
        seed_release(tmp.path(), "1.1.0", ToolchainKind::Compiler, plat());

        let src = LocalDir::new(ToolchainKind::Compiler, tmp.path());
        let mut list = src.list(plat()).unwrap();
        list.sort_by(|a, b| a.version.cmp(&b.version));

        assert_eq!(list.len(), 2);
        assert_eq!(list[0].version, "1.0.0".parse().unwrap());
        assert_eq!(list[1].version, "1.1.0".parse().unwrap());
        assert!(list[0].asset_url.starts_with("file://"));
        assert!(!list[0].asset_sha256.is_empty());
    }

    #[test]
    fn local_dir_ignores_wrong_platform() {
        let tmp = tempdir().unwrap();
        let other = Platform {
            os: Os::Linux,
            arch: Arch::X86_64,
        };
        seed_release(tmp.path(), "1.0.0", ToolchainKind::Compiler, other);

        let src = LocalDir::new(ToolchainKind::Compiler, tmp.path());
        let list = src.list(plat()).unwrap();
        assert!(
            list.is_empty(),
            "should skip release with no matching asset"
        );
    }

    #[test]
    fn local_dir_resolve_latest_picks_max_stable() {
        let tmp = tempdir().unwrap();
        seed_release(tmp.path(), "1.0.0", ToolchainKind::Runtime, plat());
        seed_release(tmp.path(), "2.0.0", ToolchainKind::Runtime, plat());
        seed_release(tmp.path(), "2.1.0-rc.1", ToolchainKind::Runtime, plat());

        let src = LocalDir::new(ToolchainKind::Runtime, tmp.path());
        let e = src.resolve(&VersionSpec::Latest, plat()).unwrap();
        assert_eq!(e.version, "2.0.0".parse().unwrap());
    }

    #[test]
    fn local_dir_resolve_exact_hits_prerelease() {
        let tmp = tempdir().unwrap();
        seed_release(tmp.path(), "2.1.0-rc.1", ToolchainKind::Runtime, plat());

        let src = LocalDir::new(ToolchainKind::Runtime, tmp.path());
        let want: Version = "2.1.0-rc.1".parse().unwrap();
        let e = src
            .resolve(&VersionSpec::Exact(want.clone()), plat())
            .unwrap();
        assert_eq!(e.version, want);
    }

    #[test]
    fn local_dir_resolve_missing_version_errors() {
        let tmp = tempdir().unwrap();
        seed_release(tmp.path(), "1.0.0", ToolchainKind::Compiler, plat());

        let src = LocalDir::new(ToolchainKind::Compiler, tmp.path());
        let err = src
            .resolve(&VersionSpec::Exact("9.9.9".parse().unwrap()), plat())
            .unwrap_err();
        assert!(matches!(err, ChannelError::NoMatch { .. }));
    }

    #[test]
    fn parse_tag_accepts_v_prefix_and_bare() {
        assert_eq!(parse_tag("v1.2.3"), Some("1.2.3".parse().unwrap()));
        assert_eq!(parse_tag("1.2.3"), Some("1.2.3".parse().unwrap()));
        assert_eq!(parse_tag("release-1.2.3"), None);
    }
}
