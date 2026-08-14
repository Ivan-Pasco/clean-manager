//! Finding the project root.
//!
//! A Clean project is a directory containing `clean.toml`. Commands like
//! `cln build` accept an optional path and otherwise mean "the project I am
//! standing in", so we walk up from a starting directory until we find that
//! marker file — the same rule `cargo` uses for `Cargo.toml`.
//!
//! We deliberately do not parse `clean.toml`; its contents belong to the
//! framework. Presence is the whole signal.

use std::path::{Path, PathBuf};

/// The file whose presence marks a directory as a Clean project root.
pub const PROJECT_MARKER: &str = "clean.toml";

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("no Clean project found in {start} or any parent directory")]
    #[allow(clippy::enum_variant_names)]
    NotFound { start: PathBuf },

    #[error("{path} does not exist")]
    NoSuchPath { path: PathBuf },

    #[error("cannot determine the current directory: {source}")]
    NoCurrentDir {
        #[source]
        source: std::io::Error,
    },
}

/// A located project root — a directory that contains `clean.toml`.
///
/// Constructing one is proof the marker file was seen, so downstream code
/// (dispatch in particular) never has to re-check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    root: PathBuf,
}

impl Project {
    /// The project root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/.cln/` — where this project's pins and lockfile live.
    pub fn cln_dir(&self) -> PathBuf {
        self.root.join(".cln")
    }

    /// `<root>/clean.toml`.
    pub fn manifest(&self) -> PathBuf {
        self.root.join(PROJECT_MARKER)
    }

    /// Locate the project containing `path`.
    ///
    /// If `path` is a file, the search starts from its parent directory.
    /// Relative paths are resolved against the current directory first, so the
    /// error message and the resulting root are both absolute.
    ///
    /// A path that does not exist is not automatically an error: the walk
    /// starts from its nearest existing ancestor, so `cln build ./not-made-yet`
    /// inside a project still finds that project and lets the framework give
    /// the real diagnostic. Only a path with no existing ancestor *and* no
    /// project above it reports [`ProjectError::NoSuchPath`].
    pub fn discover(path: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let path = path.as_ref();
        let absolute = absolutize(path)?;

        // Prefer the real path so a project reached through a symlink reports
        // the directory it actually lives in. `canonicalize` requires the path
        // to exist, so fall back to the lexical form when it does not.
        let resolved = std::fs::canonicalize(&absolute).unwrap_or(absolute);

        let start = if resolved.is_dir() {
            resolved.clone()
        } else if resolved.is_file() {
            resolved
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| resolved.clone())
        } else {
            // Nonexistent: walk up to the nearest ancestor that does exist.
            resolved
                .ancestors()
                .find(|a| a.is_dir())
                .map(Path::to_path_buf)
                .ok_or_else(|| ProjectError::NoSuchPath {
                    path: resolved.clone(),
                })?
        };

        find_project_root(&start).map(|root| Self { root }).ok_or({
            // Nothing found. If the requested path itself was missing, that is
            // the more useful thing to say.
            if resolved.exists() {
                ProjectError::NotFound { start }
            } else {
                ProjectError::NoSuchPath { path: resolved }
            }
        })
    }

    /// Treat `root` as a project root without searching upward.
    ///
    /// Used by tests and by callers that already proved the marker exists.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// Walk up from `start` looking for a directory containing `clean.toml`.
///
/// Returns the first match, or `None` at the filesystem root. The loop is
/// bounded by `Path::parent` returning `None`, so a cycle-free ancestor chain
/// is guaranteed to terminate.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(PROJECT_MARKER).is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// Make a path absolute without requiring it to exist.
fn absolutize(path: &Path) -> Result<PathBuf, ProjectError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|source| ProjectError::NoCurrentDir { source })?;
    Ok(cwd.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_project(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(PROJECT_MARKER), b"[project]\nname = \"demo\"\n").unwrap();
    }

    #[test]
    fn finds_marker_in_the_directory_itself() {
        let tmp = tempdir().unwrap();
        make_project(tmp.path());

        let p = Project::discover(tmp.path()).unwrap();
        assert!(p.manifest().is_file());
    }

    #[test]
    fn walks_up_from_a_nested_directory() {
        let tmp = tempdir().unwrap();
        make_project(tmp.path());
        let nested = tmp.path().join("src").join("deep").join("deeper");
        std::fs::create_dir_all(&nested).unwrap();

        let p = Project::discover(&nested).unwrap();
        assert_eq!(
            std::fs::canonicalize(p.root()).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
    }

    #[test]
    fn starts_from_the_parent_when_given_a_file() {
        let tmp = tempdir().unwrap();
        make_project(tmp.path());
        let file = tmp.path().join("src").join("main.cln");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"").unwrap();

        let p = Project::discover(&file).unwrap();
        assert_eq!(
            std::fs::canonicalize(p.root()).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
    }

    #[test]
    fn nearest_root_wins_when_projects_nest() {
        let tmp = tempdir().unwrap();
        make_project(tmp.path());
        let inner = tmp.path().join("vendor").join("inner");
        make_project(&inner);

        let p = Project::discover(inner.join("src")).unwrap();
        assert_eq!(
            std::fs::canonicalize(p.root()).unwrap(),
            std::fs::canonicalize(&inner).unwrap()
        );
    }

    #[test]
    fn errors_when_no_marker_anywhere() {
        let tmp = tempdir().unwrap();
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        // A tempdir under /tmp has no clean.toml ancestor, so this walks to /
        // and gives up.
        let err = Project::discover(&nested).unwrap_err();
        assert!(matches!(err, ProjectError::NotFound { .. }));
    }

    #[test]
    fn errors_for_a_path_that_does_not_exist_outside_a_project() {
        let tmp = tempdir().unwrap();
        let err = Project::discover(tmp.path().join("ghost")).unwrap_err();
        assert!(matches!(err, ProjectError::NoSuchPath { .. }));
    }

    /// `cln build ./not-made-yet` inside a project should reach the framework,
    /// which owns the diagnostic about the missing directory.
    #[test]
    fn a_missing_path_inside_a_project_still_finds_that_project() {
        let tmp = tempdir().unwrap();
        make_project(tmp.path());

        let p = Project::discover(tmp.path().join("not-made-yet")).unwrap();
        assert_eq!(
            std::fs::canonicalize(p.root()).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
    }

    #[test]
    fn a_directory_named_clean_toml_is_not_a_marker() {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(PROJECT_MARKER)).unwrap();

        let err = Project::discover(tmp.path()).unwrap_err();
        assert!(matches!(err, ProjectError::NotFound { .. }));
    }

    #[test]
    fn cln_dir_hangs_off_the_root() {
        let p = Project::at("/projects/demo");
        assert_eq!(p.cln_dir(), Path::new("/projects/demo/.cln"));
    }
}
