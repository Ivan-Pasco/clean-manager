//! Unpacking a bundle into `~/.cln/cache/run/<sha>/` (Manager §00.13 step 3).
//!
//! # The layout is the contract
//!
//! A bundle's internal arrangement is load-bearing and must survive extraction
//! byte-for-byte in *shape*, not just in content. `config/host.toml` contains:
//!
//! ```toml
//! [guest]
//! wasm = "../app.wasm"
//! ```
//!
//! and `clean-host-core` resolves that path against **the config file's own
//! directory**, not the archive root. From `config/`, the only path that
//! reaches the component is `../app.wasm`. Flatten the archive — put
//! `host.toml` beside `app.wasm` — and every structural check still passes,
//! the manifest still parses, and then the host fails looking for
//! `config/app.wasm`. So extraction mirrors the archive exactly.
//!
//! # The cache key is the archive's hash
//!
//! `<sha>` is SHA-256 over **the bundle file's bytes**, not over the component
//! inside it. The two differ precisely when the wasm is unchanged but
//! something around it moved — a regenerated `config/host.toml`, a new asset,
//! a bumped version in `manifest.toml`. Keying on the component would serve a
//! stale config for a bundle whose configuration is the only thing that
//! changed, which is both a real editing loop and a silent wrong answer.
//! Keying on the archive re-extracts in exactly those cases. It also means the
//! key is computable before the archive is opened, so a corrupt bundle is
//! caught after hashing rather than after a partial unpack.
//!
//! # Extraction is atomic
//!
//! Contents are unpacked into a sibling staging directory and renamed into
//! place. Two `cln run`s of the same bundle race only on the rename, which is
//! atomic; neither can observe the other's half-written tree. An interrupted
//! run leaves a staging directory behind rather than a cache entry that looks
//! complete and is not — the failure mode that would otherwise persist across
//! every later run of that bundle.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::manifest::{Manifest, ManifestError, MANIFEST_NAME};

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("could not read {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{} is not a readable archive: {source}", .path.display())]
    NotAnArchive {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },

    #[error("the bundle has no {MANIFEST_NAME} at its root")]
    NoManifest,

    #[error("{MANIFEST_NAME} is not valid UTF-8")]
    ManifestNotUtf8,

    #[error(transparent)]
    Manifest(#[from] ManifestError),

    #[error("the bundle contains an entry that escapes the archive root: {entry}")]
    UnsafeEntry { entry: String },
}

impl ExtractError {
    pub fn remedy(&self) -> Option<String> {
        match self {
            ExtractError::NotAnArchive { .. } | ExtractError::NoManifest => {
                Some("the bundle looks corrupt; re-download or re-run `cln package`".into())
            }
            ExtractError::UnsafeEntry { .. } => Some(
                "this bundle is malformed and was not unpacked; do not trust its source".into(),
            ),
            ExtractError::Manifest(e) => e.remedy(),
            _ => None,
        }
    }
}

/// A bundle unpacked and ready to run.
#[derive(Debug)]
pub struct Extracted {
    /// `~/.cln/cache/run/<sha>/` — the archive root, reproduced on disk.
    pub root: PathBuf,
    pub manifest: Manifest,
    /// Whether this call did the unpacking, or found it already cached.
    pub freshly_extracted: bool,
}

impl Extracted {
    /// Resolve an archive-relative path against the extracted root.
    pub fn join(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

/// Hex-lowercase SHA-256 over a file's bytes, streamed rather than buffered
/// whole — a `.serve` bundle carrying assets can be large.
pub fn file_sha256(path: &Path) -> Result<String, ExtractError> {
    let mut file = std::fs::File::open(path).map_err(|source| ExtractError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|source| ExtractError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Unpack `bundle` into `cache_root/<sha>/`, or reuse an existing extraction.
///
/// `cache_root` is `~/.cln/cache/run/`. The returned [`Extracted`] carries the
/// parsed manifest so callers do not read it twice.
pub fn extract(bundle: &Path, cache_root: &Path) -> Result<Extracted, ExtractError> {
    let sha = file_sha256(bundle)?;
    let dest = cache_root.join(&sha);

    // A cache entry exists only if it was renamed into place complete, so its
    // presence is sufficient — no partial tree can be observed under this name.
    if dest.join(MANIFEST_NAME).is_file() {
        let manifest = read_manifest_from_dir(&dest)?;
        return Ok(Extracted {
            root: dest,
            manifest,
            freshly_extracted: false,
        });
    }

    std::fs::create_dir_all(cache_root).map_err(|source| ExtractError::Io {
        path: cache_root.to_path_buf(),
        source,
    })?;

    // Staged beside the destination so the rename is same-filesystem, and
    // suffixed with the pid so two concurrent runs never share a staging dir.
    let staging = cache_root.join(format!(".{sha}.tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);

    let result = unpack_into(bundle, &staging);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    let manifest = result?;

    // Atomic publish. A concurrent run may have finished first, in which case
    // the destination already exists and its contents are identical — same
    // hash, same bytes — so ours is redundant and gets dropped.
    match std::fs::rename(&staging, &dest) {
        Ok(()) => {}
        Err(_) if dest.join(MANIFEST_NAME).is_file() => {
            let _ = std::fs::remove_dir_all(&staging);
        }
        Err(source) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(ExtractError::Io { path: dest, source });
        }
    }

    Ok(Extracted {
        root: dest,
        manifest,
        freshly_extracted: true,
    })
}

fn read_manifest_from_dir(dir: &Path) -> Result<Manifest, ExtractError> {
    let path = dir.join(MANIFEST_NAME);
    let text = std::fs::read_to_string(&path).map_err(|source| ExtractError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(Manifest::parse(&text)?)
}

/// Unpack every entry into `staging`, preserving the archive's directory
/// structure, and return the parsed manifest.
fn unpack_into(bundle: &Path, staging: &Path) -> Result<Manifest, ExtractError> {
    let file = std::fs::File::open(bundle).map_err(|source| ExtractError::Io {
        path: bundle.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|source| ExtractError::NotAnArchive {
        path: bundle.to_path_buf(),
        source,
    })?;

    std::fs::create_dir_all(staging).map_err(|source| ExtractError::Io {
        path: staging.to_path_buf(),
        source,
    })?;

    let mut manifest_text: Option<String> = None;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|source| ExtractError::NotAnArchive {
                path: bundle.to_path_buf(),
                source,
            })?;

        let name = entry.name().to_string();

        // `enclosed_name` rejects absolute paths and `..` traversal. A bundle
        // is untrusted input — it may have arrived by download or by
        // double-click — so an entry that would write outside the cache
        // directory aborts the whole extraction rather than being skipped.
        // Skipping would produce a tree that is missing a file for a reason
        // nothing downstream could diagnose.
        let relative = entry
            .enclosed_name()
            .filter(|p| is_safe_relative(p))
            .ok_or_else(|| ExtractError::UnsafeEntry {
                entry: name.clone(),
            })?
            .to_path_buf();

        let target = staging.join(&relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|source| ExtractError::Io {
                path: target.clone(),
                source,
            })?;
            continue;
        }

        // Directories are created from the file paths themselves, so an archive
        // that omits directory entries — which the framework's writer does —
        // still produces `config/` before `config/host.toml` lands in it.
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ExtractError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|source| ExtractError::Io {
                path: target.clone(),
                source,
            })?;

        if relative == Path::new(MANIFEST_NAME) {
            manifest_text =
                Some(String::from_utf8(bytes.clone()).map_err(|_| ExtractError::ManifestNotUtf8)?);
        }

        std::fs::write(&target, &bytes).map_err(|source| ExtractError::Io {
            path: target.clone(),
            source,
        })?;
    }

    let text = manifest_text.ok_or(ExtractError::NoManifest)?;
    Ok(Manifest::parse(&text)?)
}

/// Belt-and-braces over `enclosed_name`: reject anything that is not a plain
/// relative path of normal components.
fn is_safe_relative(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    /// Build a ZIP in memory from `(path, bytes)` pairs.
    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::FileOptions::default();
            for (name, bytes) in entries {
                w.start_file(*name, opts).unwrap();
                w.write_all(bytes).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    const MANIFEST: &str = r#"
spec_version = "1"
[package]
name = "hello-world"
version = "0.1.0"
[build]
runtime_version = "unknown"
[artifact]
kind = "clapp"
worlds = ["cli"]
entry_wasm = "app.wasm"
"#;

    /// The real `.clapp` shape: component at the root, config one level down
    /// pointing back up at it.
    fn hello_clapp() -> Vec<u8> {
        zip_bytes(&[
            ("manifest.toml", MANIFEST.as_bytes()),
            ("app.wasm", b"\0asm\x0d\x00\x01\x00"),
            (
                "config/host.toml",
                b"[guest]\nwasm = \"../app.wasm\"\nworld = \"cli-default\"\n",
            ),
        ])
    }

    fn write_bundle(dir: &Path, bytes: &[u8]) -> PathBuf {
        let p = dir.join("hello-world.clapp");
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// The whole reason this module exists: `../app.wasm` in the config must
    /// resolve after extraction, which only holds if `config/` survives as a
    /// directory.
    #[test]
    fn extraction_preserves_the_archives_structure() {
        let tmp = tempdir().unwrap();
        let bundle = write_bundle(tmp.path(), &hello_clapp());
        let cache = tmp.path().join("cache/run");

        let out = extract(&bundle, &cache).unwrap();

        assert!(out.join("app.wasm").is_file(), "component at the root");
        assert!(
            out.join("config/host.toml").is_file(),
            "config must stay one level down, not be flattened"
        );

        // The path the host will actually follow.
        let from_config = out.join("config").join("../app.wasm");
        assert!(
            from_config.exists(),
            "`../app.wasm` from config/ must reach the component"
        );
    }

    #[test]
    fn the_manifest_is_parsed_during_extraction() {
        let tmp = tempdir().unwrap();
        let bundle = write_bundle(tmp.path(), &hello_clapp());
        let out = extract(&bundle, &tmp.path().join("cache/run")).unwrap();

        assert_eq!(out.manifest.package.name, "hello-world");
        assert_eq!(out.manifest.entry(None).unwrap().world, "cli");
        assert!(out.freshly_extracted);
    }

    /// The cache key is the archive's hash, so the directory name is stable
    /// across runs of the same bytes.
    #[test]
    fn the_cache_directory_is_named_for_the_archive_hash() {
        let tmp = tempdir().unwrap();
        let bytes = hello_clapp();
        let bundle = write_bundle(tmp.path(), &bytes);
        let cache = tmp.path().join("cache/run");

        let out = extract(&bundle, &cache).unwrap();
        assert_eq!(
            out.root.file_name().unwrap().to_str().unwrap(),
            file_sha256(&bundle).unwrap()
        );
    }

    #[test]
    fn a_second_run_reuses_the_extraction() {
        let tmp = tempdir().unwrap();
        let bundle = write_bundle(tmp.path(), &hello_clapp());
        let cache = tmp.path().join("cache/run");

        let first = extract(&bundle, &cache).unwrap();
        assert!(first.freshly_extracted);

        let second = extract(&bundle, &cache).unwrap();
        assert!(!second.freshly_extracted, "the second run must hit cache");
        assert_eq!(first.root, second.root);
    }

    /// The reason the key is the archive hash rather than the component hash:
    /// a config-only change must land in a different cache entry, or the run
    /// silently uses the previous configuration.
    #[test]
    fn changing_only_the_config_produces_a_different_cache_entry() {
        let tmp = tempdir().unwrap();
        let cache = tmp.path().join("cache/run");

        let original = write_bundle(tmp.path(), &hello_clapp());
        let first = extract(&original, &cache).unwrap();

        // Same component, different config.
        let changed = zip_bytes(&[
            ("manifest.toml", MANIFEST.as_bytes()),
            ("app.wasm", b"\0asm\x0d\x00\x01\x00"),
            (
                "config/host.toml",
                b"[guest]\nwasm = \"../app.wasm\"\nworld = \"cli-default\"\n[host]\nname = \"clean-cli\"\n",
            ),
        ]);
        let second_path = tmp.path().join("changed.clapp");
        std::fs::write(&second_path, &changed).unwrap();
        let second = extract(&second_path, &cache).unwrap();

        assert_ne!(
            first.root, second.root,
            "a config-only change must not reuse the old extraction"
        );
        assert!(second.freshly_extracted);
    }

    #[test]
    fn an_archive_without_a_manifest_is_rejected() {
        let tmp = tempdir().unwrap();
        let bytes = zip_bytes(&[("app.wasm", b"\0asm")]);
        let bundle = write_bundle(tmp.path(), &bytes);

        let err = extract(&bundle, &tmp.path().join("cache/run")).unwrap_err();
        assert!(matches!(err, ExtractError::NoManifest));
        assert!(err.remedy().unwrap().contains("corrupt"));
    }

    #[test]
    fn a_non_archive_is_rejected_before_anything_is_written() {
        let tmp = tempdir().unwrap();
        let bundle = write_bundle(tmp.path(), b"definitely not a zip");
        let cache = tmp.path().join("cache/run");

        assert!(matches!(
            extract(&bundle, &cache).unwrap_err(),
            ExtractError::NotAnArchive { .. }
        ));
        // No cache entry may survive a failed extraction.
        let entries: Vec<_> = std::fs::read_dir(&cache)
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(entries.is_empty(), "left behind: {entries:?}");
    }

    /// Zip Slip. A bundle can arrive by download or double-click, so an entry
    /// escaping the cache directory must abort rather than be written.
    #[test]
    fn an_entry_escaping_the_root_aborts_the_extraction() {
        let tmp = tempdir().unwrap();
        let bytes = zip_bytes(&[
            ("manifest.toml", MANIFEST.as_bytes()),
            ("../../escaped.txt", b"owned"),
        ]);
        let bundle = write_bundle(tmp.path(), &bytes);

        let err = extract(&bundle, &tmp.path().join("cache/run")).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafeEntry { .. }));
        assert!(
            !tmp.path().join("escaped.txt").exists(),
            "nothing may be written outside the cache root"
        );
    }

    #[test]
    fn a_failed_extraction_leaves_no_staging_directory() {
        let tmp = tempdir().unwrap();
        let bytes = zip_bytes(&[("../escape", b"x")]);
        let bundle = write_bundle(tmp.path(), &bytes);
        let cache = tmp.path().join("cache/run");

        let _ = extract(&bundle, &cache);
        let leftovers: Vec<_> = std::fs::read_dir(&cache)
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
    }

    /// The framework's archive writer emits no directory entries, so `config/`
    /// exists only as a prefix of `config/host.toml`.
    #[test]
    fn directories_are_created_from_file_paths_alone() {
        let tmp = tempdir().unwrap();
        let bytes = zip_bytes(&[
            ("manifest.toml", MANIFEST.as_bytes()),
            ("app.wasm", b"\0asm"),
            ("assets/nested/deep/icon.png", b"png"),
            ("config/host.toml", b"[guest]\n"),
        ]);
        let bundle = write_bundle(tmp.path(), &bytes);

        let out = extract(&bundle, &tmp.path().join("cache/run")).unwrap();
        assert!(out.join("assets/nested/deep/icon.png").is_file());
        assert!(out.join("config/host.toml").is_file());
    }

    #[test]
    fn a_manifest_with_an_unsupported_spec_version_fails_extraction() {
        let tmp = tempdir().unwrap();
        let future = MANIFEST.replace(r#"spec_version = "1""#, r#"spec_version = "99""#);
        let bytes = zip_bytes(&[("manifest.toml", future.as_bytes()), ("app.wasm", b"\0asm")]);
        let bundle = write_bundle(tmp.path(), &bytes);

        let err = extract(&bundle, &tmp.path().join("cache/run")).unwrap_err();
        assert!(matches!(err, ExtractError::Manifest(_)));
        assert!(err.remedy().unwrap().contains("self-update"));
    }

    #[test]
    fn sha256_matches_a_known_value() {
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("x");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            file_sha256(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
