//! Deciding what the user pointed `cln run` at (Manager §00.13 step 1).
//!
//! Four things can be named: a bundle archive, a bare `.wasm` component, a
//! project directory, or something that is none of those.
//!
//! **Detection reads the file, it does not trust the name.** Both `.clapp` and
//! `.serve` are ZIP archives and `framework-package::file_name` currently
//! writes `.clapp` for *both* kinds — the discriminator is `manifest.toml`'s
//! `kind` field, not the extension (§00.14). An extension-driven detector would
//! therefore have to be re-taught every time the producer's naming shifts,
//! while the magic bytes have been fixed since the format was chosen. So the
//! extension is used only to give a better error when the bytes say neither.

use std::io::Read;
use std::path::{Path, PathBuf};

/// The extensions §00.14 reserves for Clean bundles. Used for diagnostics and
/// for `cln register`'s file associations — never as the primary signal.
pub const BUNDLE_EXTENSIONS: &[&str] = &["clapp", "serve"];

/// What `cln run` was pointed at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Artifact {
    /// A ZIP bundle carrying `manifest.toml` — a `.clapp` or a `.serve`.
    Bundle(PathBuf),
    /// A bare wasm component, run in interop mode with generated config.
    Wasm(PathBuf),
    /// A directory containing `clean.toml`.
    Project(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("{} does not exist", .path.display())]
    NotFound { path: PathBuf },

    #[error("{} is a directory but contains no clean.toml, so it is not a Clean project", .path.display())]
    DirectoryNotAProject { path: PathBuf },

    #[error("{} is not a Clean artifact: it is neither a bundle archive nor a wasm component", .path.display())]
    Unrecognized { path: PathBuf },

    #[error("could not read {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl DetectError {
    pub fn remedy(&self) -> Option<String> {
        match self {
            DetectError::DirectoryNotAProject { .. } => {
                Some("run `cln run` from a project directory, or point it at a .clapp".into())
            }
            DetectError::Unrecognized { path } => {
                // A bundle extension on bytes that are not a ZIP is the one
                // case where naming the likely cause helps: it is almost
                // always a truncated download or a partially written file.
                let named_a_bundle = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| BUNDLE_EXTENSIONS.contains(&e));
                if named_a_bundle {
                    Some("the file's name says bundle but its contents do not; it may be truncated or corrupt".into())
                } else {
                    Some(
                        "expected a .clapp bundle, a .wasm component, or a project directory"
                            .into(),
                    )
                }
            }
            _ => None,
        }
    }
}

/// Classify the path the user named.
pub fn detect(path: &Path) -> Result<Artifact, DetectError> {
    if !path.exists() {
        return Err(DetectError::NotFound {
            path: path.to_path_buf(),
        });
    }

    if path.is_dir() {
        return if path.join(cln_project::discover::PROJECT_MARKER).is_file() {
            Ok(Artifact::Project(path.to_path_buf()))
        } else {
            Err(DetectError::DirectoryNotAProject {
                path: path.to_path_buf(),
            })
        };
    }

    match magic(path)? {
        Magic::Zip => Ok(Artifact::Bundle(path.to_path_buf())),
        Magic::Wasm => Ok(Artifact::Wasm(path.to_path_buf())),
        Magic::Neither => Err(DetectError::Unrecognized {
            path: path.to_path_buf(),
        }),
    }
}

enum Magic {
    Zip,
    Wasm,
    Neither,
}

/// Read the first four bytes and match them against the two formats §00.14
/// admits.
///
/// `PK\x03\x04` is a ZIP local file header; `\0asm` is the WebAssembly binary
/// preamble. A file shorter than four bytes is neither, and is reported as
/// unrecognized rather than as an I/O error — a zero-length download is a
/// content problem, not a permissions one.
fn magic(path: &Path) -> Result<Magic, DetectError> {
    let mut file = std::fs::File::open(path).map_err(|source| DetectError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let mut head = [0u8; 4];
    let mut filled = 0;
    // A single `read` may return fewer bytes than asked for even when more are
    // available, so loop until the buffer is full or the file ends.
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(DetectError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        }
    }

    if filled < 4 {
        return Ok(Magic::Neither);
    }

    Ok(match &head {
        b"PK\x03\x04" => Magic::Zip,
        b"\0asm" => Magic::Wasm,
        _ => Magic::Neither,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn a_zip_is_a_bundle_whatever_it_is_called() {
        let tmp = tempdir().unwrap();
        for name in ["app.clapp", "svc.serve", "mystery.bin"] {
            let p = write(tmp.path(), name, b"PK\x03\x04rest of the archive");
            assert_eq!(
                detect(&p).unwrap(),
                Artifact::Bundle(p.clone()),
                "{name} should detect as a bundle by its bytes"
            );
        }
    }

    /// The rule this crate rests on: `.clapp` and `.serve` are the same
    /// container, distinguished by the manifest inside, so detection must not
    /// branch on the extension.
    #[test]
    fn a_serve_bundle_named_clapp_still_detects_as_a_bundle() {
        let tmp = tempdir().unwrap();
        let p = write(tmp.path(), "server-bundle.clapp", b"PK\x03\x04...");
        assert!(matches!(detect(&p).unwrap(), Artifact::Bundle(_)));
    }

    #[test]
    fn the_wasm_preamble_is_a_bare_component() {
        let tmp = tempdir().unwrap();
        let p = write(tmp.path(), "app.wasm", b"\0asm\x0d\x00\x01\x00");
        assert_eq!(detect(&p).unwrap(), Artifact::Wasm(p));
    }

    /// A `.wasm` extension over non-wasm bytes must not be trusted into the
    /// runtime, which would fail much later and less clearly.
    #[test]
    fn a_wasm_extension_over_wrong_bytes_is_unrecognized() {
        let tmp = tempdir().unwrap();
        let p = write(tmp.path(), "app.wasm", b"#!/bin/sh\necho no\n");
        assert!(matches!(detect(&p), Err(DetectError::Unrecognized { .. })));
    }

    #[test]
    fn a_directory_with_clean_toml_is_a_project() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("clean.toml"), b"[project]\n").unwrap();
        assert_eq!(
            detect(tmp.path()).unwrap(),
            Artifact::Project(tmp.path().to_path_buf())
        );
    }

    #[test]
    fn a_directory_without_clean_toml_says_so() {
        let tmp = tempdir().unwrap();
        let err = detect(tmp.path()).unwrap_err();
        assert!(matches!(err, DetectError::DirectoryNotAProject { .. }));
        assert!(err.remedy().unwrap().contains("project directory"));
    }

    #[test]
    fn a_missing_path_is_reported_before_anything_else() {
        let tmp = tempdir().unwrap();
        let err = detect(&tmp.path().join("ghost.clapp")).unwrap_err();
        assert!(matches!(err, DetectError::NotFound { .. }));
    }

    /// A truncated download is the common cause, and the remedy should say so
    /// rather than making the user guess.
    #[test]
    fn a_bundle_extension_over_junk_suggests_corruption() {
        let tmp = tempdir().unwrap();
        let p = write(tmp.path(), "app.clapp", b"not an archive at all");
        let err = detect(&p).unwrap_err();
        assert!(err.remedy().unwrap().contains("truncated or corrupt"));
    }

    #[test]
    fn an_empty_file_is_unrecognized_not_an_io_error() {
        let tmp = tempdir().unwrap();
        let p = write(tmp.path(), "empty.clapp", b"");
        assert!(matches!(detect(&p), Err(DetectError::Unrecognized { .. })));
    }

    #[test]
    fn a_file_shorter_than_the_magic_does_not_panic() {
        let tmp = tempdir().unwrap();
        let p = write(tmp.path(), "tiny.wasm", b"\0as");
        assert!(matches!(detect(&p), Err(DetectError::Unrecognized { .. })));
    }
}
