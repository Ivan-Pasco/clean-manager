//! Fetch a release asset to a content-addressed path under
//! `~/.cln/cache/downloads/`, streaming and verifying the SHA-256 as we go.
//!
//! - `file://` URLs are supported so tests exercise the same code path.
//! - Verified blobs live at `<downloads>/<sha256>` — reused across installs
//!   of the same version, and safe to prune anytime (they can be re-fetched).

use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use cln_layout::Layout;
use cln_shared::ReleaseEntry;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("network error fetching {url}: {source}")]
    Network { url: String, #[source] source: Box<ureq::Error> },
    #[error("io error writing to {path}: {source}")]
    Io { path: PathBuf, #[source] source: io::Error },
    #[error("SHA-256 mismatch for {url}: expected {expected}, got {actual}")]
    ChecksumMismatch { url: String, expected: String, actual: String },
    #[error("unsupported URL scheme for {0}")]
    UnsupportedScheme(String),
}

/// Ensure the asset described by `entry` is on disk. Returns the verified
/// local path. Idempotent — a matching file at the cache path is trusted
/// after a re-verify.
pub fn fetch(layout: &Layout, entry: &ReleaseEntry) -> Result<PathBuf, DownloadError> {
    fs::create_dir_all(layout.downloads_dir()).map_err(|source| DownloadError::Io {
        path: layout.downloads_dir(),
        source,
    })?;
    let dest = layout.downloads_dir().join(&entry.asset_sha256);

    if dest.exists() {
        let actual = hash_file(&dest).map_err(|source| DownloadError::Io {
            path: dest.clone(),
            source,
        })?;
        if actual == entry.asset_sha256.to_ascii_lowercase() {
            return Ok(dest);
        }
        // Stale/corrupt — remove and refetch.
        let _ = fs::remove_file(&dest);
    }

    // Stream to a unique temp file in the same dir, then rename on success.
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = layout
        .downloads_dir()
        .join(format!(".{}.tmp.{pid}.{nanos}", entry.asset_sha256));

    let actual = stream_and_hash(&entry.asset_url, &tmp)?;
    if actual != entry.asset_sha256.to_ascii_lowercase() {
        let _ = fs::remove_file(&tmp);
        return Err(DownloadError::ChecksumMismatch {
            url: entry.asset_url.clone(),
            expected: entry.asset_sha256.clone(),
            actual,
        });
    }
    fs::rename(&tmp, &dest).map_err(|source| DownloadError::Io {
        path: dest.clone(),
        source,
    })?;
    Ok(dest)
}

fn stream_and_hash(url: &str, dest: &std::path::Path) -> Result<String, DownloadError> {
    let mut hasher = Sha256::new();
    let mut file = fs::File::create(dest).map_err(|source| DownloadError::Io {
        path: dest.into(),
        source,
    })?;

    let reader: Box<dyn Read + Send> = if let Some(path) = strip_file_url(url) {
        let f = fs::File::open(&path).map_err(|source| DownloadError::Io {
            path: path.clone(),
            source,
        })?;
        Box::new(f)
    } else if url.starts_with("http://") || url.starts_with("https://") {
        let resp = ureq::get(url)
            .set("User-Agent", "cln-manager")
            .call()
            .map_err(|e| DownloadError::Network { url: url.into(), source: Box::new(e) })?;
        Box::new(resp.into_reader())
    } else {
        return Err(DownloadError::UnsupportedScheme(url.into()));
    };

    let mut buf = [0u8; 64 * 1024];
    let mut reader = reader;
    loop {
        let n = reader.read(&mut buf).map_err(|source| DownloadError::Io {
            path: dest.into(),
            source,
        })?;
        if n == 0 { break }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|source| DownloadError::Io {
            path: dest.into(),
            source,
        })?;
    }
    file.flush().map_err(|source| DownloadError::Io { path: dest.into(), source })?;

    Ok(to_hex(&hasher.finalize()))
}

fn hash_file(path: &std::path::Path) -> io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

fn strip_file_url(url: &str) -> Option<PathBuf> {
    url.strip_prefix("file://").map(PathBuf::from)
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes { s.push_str(&format!("{b:02x}")); }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use cln_shared::platform::{Arch, Os};
    use cln_shared::{Platform, ToolchainKind};
    use tempfile::tempdir;

    fn write_asset(dir: &std::path::Path, name: &str, bytes: &[u8]) -> (PathBuf, String) {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        (path, to_hex(Sha256::digest(bytes).as_slice()))
    }

    fn entry_for(url: &str, sha: &str) -> ReleaseEntry {
        ReleaseEntry {
            kind: ToolchainKind::Compiler,
            version: "1.0.0".parse().unwrap(),
            platform: Platform { os: Os::Macos, arch: Arch::Arm64 },
            asset_url: url.into(),
            asset_sha256: sha.into(),
            compatibility: None,
        }
    }

    #[test]
    fn fetches_file_url_and_verifies() {
        let staging = tempdir().unwrap();
        let (asset, sha) = write_asset(staging.path(), "artifact.bin", b"hello world");
        let layout_root = tempdir().unwrap();
        let layout = Layout::new(layout_root.path());
        layout.ensure_base().unwrap();

        let entry = entry_for(&format!("file://{}", asset.display()), &sha);
        let dest = fetch(&layout, &entry).unwrap();

        assert_eq!(dest, layout.downloads_dir().join(&sha));
        assert_eq!(fs::read(&dest).unwrap(), b"hello world");
    }

    #[test]
    fn detects_checksum_mismatch() {
        let staging = tempdir().unwrap();
        let (asset, _real_sha) = write_asset(staging.path(), "artifact.bin", b"payload");
        let layout_root = tempdir().unwrap();
        let layout = Layout::new(layout_root.path());
        layout.ensure_base().unwrap();

        let entry = entry_for(
            &format!("file://{}", asset.display()),
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        let err = fetch(&layout, &entry).unwrap_err();
        assert!(matches!(err, DownloadError::ChecksumMismatch { .. }));

        // Temp file must not linger.
        let leftover: Vec<_> = fs::read_dir(layout.downloads_dir())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().into_string().unwrap()))
            .collect();
        assert!(
            leftover.is_empty(),
            "download dir should be empty after mismatch, got {leftover:?}"
        );
    }

    #[test]
    fn reuses_cached_download() {
        let staging = tempdir().unwrap();
        let (asset, sha) = write_asset(staging.path(), "artifact.bin", b"reuse");
        let layout_root = tempdir().unwrap();
        let layout = Layout::new(layout_root.path());
        layout.ensure_base().unwrap();

        let entry = entry_for(&format!("file://{}", asset.display()), &sha);
        let first = fetch(&layout, &entry).unwrap();
        // Second call must return the same path without touching the source.
        fs::remove_file(&asset).unwrap();
        let second = fetch(&layout, &entry).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn refetches_when_cached_blob_is_corrupt() {
        let staging = tempdir().unwrap();
        let (asset, sha) = write_asset(staging.path(), "artifact.bin", b"good");
        let layout_root = tempdir().unwrap();
        let layout = Layout::new(layout_root.path());
        layout.ensure_base().unwrap();

        let entry = entry_for(&format!("file://{}", asset.display()), &sha);
        let dest = fetch(&layout, &entry).unwrap();

        // Corrupt the cached blob in place.
        fs::write(&dest, b"corrupt").unwrap();
        let dest2 = fetch(&layout, &entry).unwrap();
        assert_eq!(dest2, dest);
        assert_eq!(fs::read(&dest2).unwrap(), b"good");
    }
}
