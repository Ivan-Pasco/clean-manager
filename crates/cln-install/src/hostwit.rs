//! Seeding `~/.cln/host-wit/` with the host contracts we ship in the binary.
//!
//! # Why the contracts are embedded rather than fetched
//!
//! C-18 promises every command works offline. A project's *first* `cln build`
//! needs the target host's `host.wit` to validate the guest against (Moment 1),
//! and on a cold cache there is nothing to read. Fetching at install time would
//! move the network round trip earlier without removing it — an install on a
//! metered or air-gapped machine would still leave the cache empty. So the
//! contracts travel inside the `cln` binary via `include_str!` and land on disk
//! during `cln install`, before anything needs them.
//!
//! # Byte-for-byte, always
//!
//! The framework hashes the contract it reads and pins that hash into the
//! project's lock file (BVER-03). Normalizing anything — line endings, a
//! trailing newline, comment stripping — changes the hash and breaks every
//! pinned project. We write exactly the bytes the host published, and verify
//! that is what we did (C-17) before the file lands.
//!
//! See `vendor/host-wit/README.md` for provenance and the drift check.

use std::path::{Path, PathBuf};

use cln_layout::Layout;
use sha2::{Digest, Sha256};

/// A host contract compiled into this binary.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Contract {
    /// Host name, e.g. `clean-server`. Names the cache entry, so it must match
    /// the key in the framework's official-host registry exactly.
    pub host: &'static str,
    /// The host's release version, e.g. `0.7.0`. Not the WIT package version.
    pub version: &'static str,
    /// The contract text, byte-for-byte as the host published it.
    pub wit: &'static str,
    /// SHA-256 of `wit`, lowercase hex. Pinned at compile time and re-checked
    /// before every write.
    pub sha256: &'static str,
}

/// Every contract this build of `cln` ships.
///
/// `clean-cli` is deliberately absent: no CLI host is implemented yet, and
/// HCV-06 forbids declaring an interface nothing fulfills. It gets an entry
/// here when `clean-runtime` actually implements `clean:host@0.1.0`.
pub const CONTRACTS: &[Contract] = &[Contract {
    host: "clean-server",
    version: "0.7.0",
    wit: include_str!("../vendor/host-wit/clean-server@0.7.0.wit"),
    sha256: "c4aaba83494e63577cb798e1483ce6604c6e55660010c5d0ced3be0d2a6963de",
}];

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    /// The embedded bytes do not hash to the pinned constant. Either the
    /// vendored file was edited without updating the constant, or the binary
    /// is corrupt. Both are install-stopping (C-17).
    #[error(
        "embedded contract {host}@{version} does not match its pinned hash \
         (expected {expected}, computed {actual}) — the vendored .wit was \
         edited without updating CONTRACTS, or this binary is corrupt"
    )]
    HashMismatch {
        host: &'static str,
        version: &'static str,
        expected: &'static str,
        actual: String,
    },

    /// A cache entry already exists with different content. A published
    /// `<host>@<version>` is immutable, so this is either a republished
    /// contract or a tampered cache — both need a human. We leave it alone.
    #[error(
        "{path} already exists with different content — a published contract \
         is immutable, so this is either a republished {host}@{version} or a \
         modified cache entry; inspect it and remove it to re-seed"
    )]
    Conflict {
        host: &'static str,
        version: &'static str,
        path: PathBuf,
    },

    #[error("io error seeding {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What happened to one contract during seeding.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Seeded {
    pub host: &'static str,
    pub version: &'static str,
    /// Where it landed (or already was).
    pub path: PathBuf,
    /// True when the file was already present with identical content, so
    /// nothing was written.
    pub already_present: bool,
}

impl Seeded {
    /// `clean-server@0.7.0` — the cache key, for reporting.
    pub fn label(&self) -> String {
        format!("{}@{}", self.host, self.version)
    }
}

/// Write every embedded contract into `~/.cln/host-wit/`.
///
/// Idempotent: a contract already on disk with identical bytes is left
/// untouched and reported as `already_present`. Contracts are toolchain-wide,
/// not per-kind, so callers installing several kinds should seed once.
pub fn seed_all(layout: &Layout) -> Result<Vec<Seeded>, SeedError> {
    let dir = layout.host_wit_dir();
    std::fs::create_dir_all(&dir).map_err(|source| SeedError::Io {
        path: dir.clone(),
        source,
    })?;

    CONTRACTS.iter().map(|c| seed_one(&dir, c)).collect()
}

fn seed_one(dir: &Path, contract: &Contract) -> Result<Seeded, SeedError> {
    // C-17: verify the bytes we are about to write are the bytes we promised.
    let actual = sha256_hex(contract.wit.as_bytes());
    if actual != contract.sha256 {
        return Err(SeedError::HashMismatch {
            host: contract.host,
            version: contract.version,
            expected: contract.sha256,
            actual,
        });
    }

    // Matches HostWitCache::path_for in framework-core exactly. Diverging here
    // would seed files the framework never looks for.
    let path = dir.join(format!("{}@{}.wit", contract.host, contract.version));

    match std::fs::read(&path) {
        Ok(existing) if existing == contract.wit.as_bytes() => {
            return Ok(Seeded {
                host: contract.host,
                version: contract.version,
                path,
                already_present: true,
            })
        }
        // Never clobber a differing contract — see SeedError::Conflict.
        Ok(_) => {
            return Err(SeedError::Conflict {
                host: contract.host,
                version: contract.version,
                path,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(SeedError::Io { path, source }),
    }

    // Atomic: stage then rename, so a concurrent `cln build` never reads a
    // half-written contract and takes its hash. Mirrors HostWitCache::put.
    let staging = dir.join(format!(
        ".{}@{}.wit.tmp-{}",
        contract.host,
        contract.version,
        std::process::id()
    ));
    std::fs::write(&staging, contract.wit).map_err(|source| SeedError::Io {
        path: staging.clone(),
        source,
    })?;
    if let Err(source) = std::fs::rename(&staging, &path) {
        let _ = std::fs::remove_file(&staging);
        return Err(SeedError::Io { path, source });
    }

    Ok(Seeded {
        host: contract.host,
        version: contract.version,
        path,
        already_present: false,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn layout() -> (tempfile::TempDir, Layout) {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path().join(".cln"));
        (tmp, l)
    }

    #[test]
    fn every_embedded_contract_matches_its_pinned_hash() {
        for c in CONTRACTS {
            assert_eq!(
                sha256_hex(c.wit.as_bytes()),
                c.sha256,
                "{}@{} hash drifted — vendored file edited without updating CONTRACTS",
                c.host,
                c.version
            );
        }
    }

    #[test]
    fn clean_server_contract_is_pinned_at_the_ratified_hash() {
        // The hash clean-server published for v0.7.0 (commit 54ca10d). If this
        // fails, the vendored copy is not the contract the ecosystem pins.
        let c = CONTRACTS.iter().find(|c| c.host == "clean-server").unwrap();
        assert_eq!(c.version, "0.7.0");
        assert_eq!(
            c.sha256,
            "c4aaba83494e63577cb798e1483ce6604c6e55660010c5d0ced3be0d2a6963de"
        );
    }

    #[test]
    fn contracts_declare_the_single_host_package() {
        // CMOD-01: exactly one package for host contracts.
        for c in CONTRACTS {
            assert!(
                c.wit.contains("package clean:host@0.1.0;"),
                "{} must declare package clean:host@0.1.0",
                c.host
            );
        }
    }

    #[test]
    fn no_contract_is_published_for_an_unimplemented_host() {
        // HCV-06: a declared-but-unimplemented interface is a hard failure.
        // No CLI host exists yet, so clean-cli must not appear here.
        assert!(
            !CONTRACTS.iter().any(|c| c.host == "clean-cli"),
            "clean-cli has no implementation; publishing its contract violates HCV-06"
        );
    }

    #[test]
    fn seed_writes_bytes_identical_to_the_embedded_contract() {
        let (_tmp, l) = layout();
        let seeded = seed_all(&l).unwrap();
        assert_eq!(seeded.len(), CONTRACTS.len());

        for (s, c) in seeded.iter().zip(CONTRACTS) {
            assert!(!s.already_present);
            assert_eq!(std::fs::read(&s.path).unwrap(), c.wit.as_bytes());
        }
    }

    #[test]
    fn seed_path_matches_the_framework_cache_layout() {
        let (_tmp, l) = layout();
        seed_all(&l).unwrap();
        // framework-core HostWitCache::path_for: root.join("{host}@{version}.wit")
        assert!(l.host_wit_dir().join("clean-server@0.7.0.wit").is_file());
    }

    #[test]
    fn seed_is_idempotent_and_reports_already_present() {
        let (_tmp, l) = layout();
        seed_all(&l).unwrap();
        let second = seed_all(&l).unwrap();
        assert!(second.iter().all(|s| s.already_present));
    }

    #[test]
    fn seed_refuses_to_overwrite_a_differing_contract() {
        let (_tmp, l) = layout();
        seed_all(&l).unwrap();

        let path = l.host_wit_dir().join("clean-server@0.7.0.wit");
        std::fs::write(&path, "tampered").unwrap();

        let err = seed_all(&l).unwrap_err();
        assert!(matches!(
            err,
            SeedError::Conflict {
                host: "clean-server",
                ..
            }
        ));
        // The existing file is left exactly as found — no clobber.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "tampered");
    }

    #[test]
    fn seed_creates_the_directory_when_absent() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path().join(".cln"));
        // No ensure_base() — seeding must stand on its own.
        seed_all(&l).unwrap();
        assert!(l.host_wit_dir().is_dir());
    }

    #[test]
    fn seed_leaves_no_staging_files_behind() {
        let (_tmp, l) = layout();
        seed_all(&l).unwrap();
        let strays: Vec<_> = std::fs::read_dir(l.host_wit_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.') || n.contains(".tmp-"))
            .collect();
        assert!(strays.is_empty(), "staging files left behind: {strays:?}");
    }
}
