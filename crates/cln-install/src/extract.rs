//! Extract a downloaded archive into `~/.cln/versions/<kind>/<version>/`.
//!
//! Detection is by URL suffix (matching `Platform::archive_ext`). Both formats
//! extract into a temp sibling of the final dir; on success we atomic-rename
//! that sibling into place. On failure we clean up so a retry starts clean.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("io error while extracting {archive}: {source}")]
    Io {
        archive: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("archive format not recognized for {0}")]
    UnknownFormat(String),
    #[error("archive {archive} did not contain expected binary {expected}")]
    MissingBinary { archive: PathBuf, expected: String },
    #[error("zip extract failed for {archive}: {source}")]
    Zip {
        archive: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },
}

/// Extract `archive` into `dest_dir`. `dest_dir` will be created; if it
/// already exists, it's removed first. `hint_name` is a display-only string
/// used in error messages to identify the source (e.g. the URL).
///
/// After extraction, `expected_binary` (relative to `dest_dir`) must exist —
/// otherwise the extraction is treated as failed and cleaned up.
pub fn extract_archive(
    archive: &Path,
    dest_dir: &Path,
    hint_name: &str,
    expected_binary: &str,
) -> Result<(), ExtractError> {
    let parent = dest_dir.parent().ok_or_else(|| ExtractError::Io {
        archive: archive.into(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "dest_dir has no parent"),
    })?;
    fs::create_dir_all(parent).map_err(|source| ExtractError::Io {
        archive: archive.into(),
        source,
    })?;

    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file_name = dest_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("version");
    let tmp = parent.join(format!(".{file_name}.extract.{pid}.{nanos}"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|source| ExtractError::Io {
        archive: archive.into(),
        source,
    })?;

    let extract_result = if hint_name.ends_with(".tar.gz") || hint_name.ends_with(".tgz") {
        extract_tar_gz(archive, &tmp)
    } else if hint_name.ends_with(".zip") {
        extract_zip(archive, &tmp)
    } else {
        Err(ExtractError::UnknownFormat(hint_name.into()))
    };

    if let Err(e) = extract_result {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }

    // Some archives wrap everything in a single top-level directory. If so,
    // collapse it: promote its children up one level.
    collapse_single_root(&tmp).map_err(|source| ExtractError::Io {
        archive: archive.into(),
        source,
    })?;

    // Sanity: the expected binary must be present.
    if !tmp.join(expected_binary).exists() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(ExtractError::MissingBinary {
            archive: archive.into(),
            expected: expected_binary.into(),
        });
    }

    // Ensure the binary is executable on Unix. Archives from GitHub Actions
    // frequently lose the +x bit; fixing it here is cheap and predictable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bin = tmp.join(expected_binary);
        if let Ok(meta) = fs::metadata(&bin) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            let _ = fs::set_permissions(&bin, perms);
        }
    }

    // Atomic rename into the final location, replacing any prior install.
    if dest_dir.exists() {
        fs::remove_dir_all(dest_dir).map_err(|source| ExtractError::Io {
            archive: archive.into(),
            source,
        })?;
    }
    fs::rename(&tmp, dest_dir).map_err(|source| ExtractError::Io {
        archive: archive.into(),
        source,
    })?;
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<(), ExtractError> {
    let f = fs::File::open(archive).map_err(|source| ExtractError::Io {
        archive: archive.into(),
        source,
    })?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut tar = tar::Archive::new(gz);
    tar.unpack(dest).map_err(|source| ExtractError::Io {
        archive: archive.into(),
        source,
    })
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<(), ExtractError> {
    let f = fs::File::open(archive).map_err(|source| ExtractError::Io {
        archive: archive.into(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(f).map_err(|source| ExtractError::Zip {
        archive: archive.into(),
        source,
    })?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|source| ExtractError::Zip {
            archive: archive.into(),
            source,
        })?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out_path = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out_path).map_err(|source| ExtractError::Io {
                archive: archive.into(),
                source,
            })?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|source| ExtractError::Io {
                    archive: archive.into(),
                    source,
                })?;
            }
            let mut out = fs::File::create(&out_path).map_err(|source| ExtractError::Io {
                archive: archive.into(),
                source,
            })?;
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry
                .read_to_end(&mut buf)
                .map_err(|source| ExtractError::Io {
                    archive: archive.into(),
                    source,
                })?;
            io::Write::write_all(&mut out, &buf).map_err(|source| ExtractError::Io {
                archive: archive.into(),
                source,
            })?;
        }
    }
    Ok(())
}

fn collapse_single_root(dir: &Path) -> io::Result<()> {
    let entries: Vec<_> = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    if entries.len() != 1 {
        return Ok(());
    }
    let only = &entries[0];
    if !only.file_type()?.is_dir() {
        return Ok(());
    }
    // Move every child of `only` up into `dir`.
    let src = only.path();
    for child in fs::read_dir(&src)? {
        let child = child?;
        let from = child.path();
        let to = dir.join(child.file_name());
        fs::rename(&from, &to)?;
    }
    fs::remove_dir(&src)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use tempfile::tempdir;

    fn make_tar_gz(dest: &Path, files: &[(&str, &[u8])]) {
        let f = fs::File::create(dest).unwrap();
        let gz = GzEncoder::new(f, Compression::default());
        let mut tar = tar::Builder::new(gz);
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, name, &bytes[..]).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn extract_tar_gz_flat_layout() {
        let staging = tempdir().unwrap();
        let archive = staging.path().join("bundle.tar.gz");
        make_tar_gz(&archive, &[("clean-compiler", b"binary bytes")]);

        let target = staging.path().join("out");
        extract_archive(&archive, &target, "bundle.tar.gz", "clean-compiler").unwrap();

        assert_eq!(
            fs::read(target.join("clean-compiler")).unwrap(),
            b"binary bytes"
        );
    }

    #[test]
    fn extract_tar_gz_collapses_single_root_dir() {
        let staging = tempdir().unwrap();
        let archive = staging.path().join("bundle.tar.gz");
        make_tar_gz(
            &archive,
            &[("clean-compiler-1.0.0/clean-compiler", b"nested binary")],
        );

        let target = staging.path().join("out");
        extract_archive(&archive, &target, "bundle.tar.gz", "clean-compiler").unwrap();

        assert!(target.join("clean-compiler").is_file());
        assert!(!target.join("clean-compiler-1.0.0").exists());
    }

    #[test]
    fn extract_missing_binary_errors_and_cleans_up() {
        let staging = tempdir().unwrap();
        let archive = staging.path().join("bundle.tar.gz");
        make_tar_gz(&archive, &[("readme.txt", b"not a binary")]);

        let target = staging.path().join("out");
        let err =
            extract_archive(&archive, &target, "bundle.tar.gz", "clean-compiler").unwrap_err();
        assert!(matches!(err, ExtractError::MissingBinary { .. }));
        assert!(
            !target.exists(),
            "failed extraction must not leave partial dest"
        );
    }

    #[test]
    fn extract_replaces_existing_dest() {
        let staging = tempdir().unwrap();
        let archive = staging.path().join("bundle.tar.gz");
        make_tar_gz(&archive, &[("clean-compiler", b"new")]);

        let target = staging.path().join("out");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("stale.txt"), b"old").unwrap();

        extract_archive(&archive, &target, "bundle.tar.gz", "clean-compiler").unwrap();
        assert!(!target.join("stale.txt").exists());
        assert_eq!(fs::read(target.join("clean-compiler")).unwrap(), b"new");
    }

    #[test]
    fn unknown_extension_errors() {
        let staging = tempdir().unwrap();
        let archive = staging.path().join("bundle.rar");
        fs::write(&archive, b"nope").unwrap();
        let target = staging.path().join("out");
        let err = extract_archive(&archive, &target, "bundle.rar", "clean-compiler").unwrap_err();
        assert!(matches!(err, ExtractError::UnknownFormat(_)));
    }
}
