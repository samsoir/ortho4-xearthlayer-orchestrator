//! Site-specific Ortho4XP config files. Ortho4XP reads some install-root
//! files (`overpass_servers.txt`, `community_server.txt`) with no variable
//! to point elsewhere; the operator supplies a directory of replacements and
//! the supervisor copies them over the install root at startup.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("config overlay {path} is not a readable directory: {source}")]
    Unreadable { path: PathBuf, source: io::Error },
    #[error("cannot install {name} over the install root: {source}")]
    Install { name: String, source: io::Error },
}

/// Copies every regular top-level `*.txt` of `overlay` over
/// `install_root/<name>`, returning the installed names (sorted). Anything
/// else is skipped with a warning naming it.
pub fn apply(overlay: &Path, install_root: &Path) -> Result<Vec<String>, OverlayError> {
    let unreadable = |source| OverlayError::Unreadable {
        path: overlay.to_path_buf(),
        source,
    };
    let mut entries = fs::read_dir(overlay)
        .map_err(unreadable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unreadable)?;
    entries.sort_by_key(|e| e.file_name());

    let mut installed = Vec::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        // file_type() does not follow symlinks, so a link never reads as a file.
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file || !name.ends_with(".txt") {
            tracing::warn!(%name, "o4 config overlay: skipped (only regular top-level *.txt files are installed)");
            continue;
        }
        if !install_root.join(&name).exists() {
            tracing::warn!(%name, "o4 config overlay: no such file in the install root; installing anyway (typo?)");
        }
        fs::copy(entry.path(), install_root.join(&name)).map_err(|source| {
            OverlayError::Install {
                name: name.clone(),
                source,
            }
        })?;
        tracing::info!("o4 config overlay: installed {name}");
        installed.push(name);
    }
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    struct Env {
        _d: tempfile::TempDir,
        overlay: PathBuf,
        install: PathBuf,
    }

    fn env() -> Env {
        let d = tempfile::tempdir().unwrap();
        let overlay = d.path().join("overlay");
        let install = d.path().join("install");
        fs::create_dir_all(&overlay).unwrap();
        fs::create_dir_all(&install).unwrap();
        Env {
            _d: d,
            overlay,
            install,
        }
    }

    #[test]
    fn a_txt_file_replaces_the_install_copy() {
        let e = env();
        fs::write(e.install.join("overpass_servers.txt"), "public").unwrap();
        fs::write(e.overlay.join("overpass_servers.txt"), "local").unwrap();
        // The "o4 config overlay: installed <name>" log line is not
        // asserted: no capture subscriber is wired in this crate.
        let done = apply(&e.overlay, &e.install).unwrap();
        assert_eq!(done, vec!["overpass_servers.txt"]);
        assert_eq!(
            fs::read_to_string(e.install.join("overpass_servers.txt")).unwrap(),
            "local"
        );
    }

    #[test]
    fn a_txt_file_absent_from_the_install_is_still_installed_with_a_warning() {
        // The warn is not asserted (no capture subscriber); the install must
        // still succeed.
        let e = env();
        fs::write(e.overlay.join("community_server.txt"), "x").unwrap();
        apply(&e.overlay, &e.install).unwrap();
        assert!(e.install.join("community_server.txt").is_file());
    }

    #[test]
    fn non_txt_subdirs_and_symlinks_are_skipped_untouched() {
        let e = env();
        fs::write(e.install.join("keep.txt"), "orig").unwrap();
        fs::write(e.install.join("notes.cfg"), "orig").unwrap();
        fs::write(e.overlay.join("notes.cfg"), "new").unwrap();
        fs::create_dir(e.overlay.join("sub.txt")).unwrap();
        fs::create_dir(e.overlay.join("dir")).unwrap();
        fs::write(e.overlay.join("dir/nested.txt"), "new").unwrap();
        let target = e.overlay.join("real_target");
        fs::write(&target, "new").unwrap();
        symlink(&target, e.overlay.join("keep.txt")).unwrap();
        let done = apply(&e.overlay, &e.install).unwrap();
        assert!(done.is_empty());
        assert_eq!(
            fs::read_to_string(e.install.join("keep.txt")).unwrap(),
            "orig"
        );
        assert_eq!(
            fs::read_to_string(e.install.join("notes.cfg")).unwrap(),
            "orig"
        );
        assert!(!e.install.join("sub.txt").exists());
        assert!(!e.install.join("dir").exists());
        assert!(!e.install.join("nested.txt").exists());
    }

    #[test]
    fn a_missing_overlay_dir_is_an_error() {
        let e = env();
        let err = apply(&e.overlay.join("nope"), &e.install).unwrap_err();
        assert!(matches!(err, OverlayError::Unreadable { .. }));
    }
}
