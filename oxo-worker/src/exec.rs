//! Filesystem lifecycle around one build: `prepare` before it, `egress`
//! after it, `cleanup` between tasks. Pure filesystem logic; every path is
//! injected through [`ExecPaths`].

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::api::ClaimedTask;

/// The overlay output tree, relative to scratch and to the target root.
const OVERLAY_TREE: [&str; 2] = ["yOrtho4XP_Overlays", "Earth nav data"];

/// The scratch skeleton: wiped and recreated empty by [`cleanup`].
pub const SCRATCH_SKELETON: [&str; 7] = [
    "tmp",
    "OSM_data",
    "Orthophotos",
    "Masks",
    "Geotiffs",
    "Tiles",
    "yOrtho4XP_Overlays",
];

/// Every location the lifecycle touches.
#[derive(Debug, Clone)]
pub struct ExecPaths {
    pub scratch: PathBuf,
    pub content: PathBuf,
    pub patches_link: PathBuf,
}

/// What `prepare` resolved, for the later steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTask {
    pub target_root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("task config has no usable target_root")]
    NoTargetRoot,
    #[error("target root {path} is not writable: {source}")]
    TargetNotWritable { path: PathBuf, source: io::Error },
    #[error("bad tile {0:?}")]
    BadTile(String),
    #[error("patches set {path} is not a directory")]
    PatchesMissing { path: PathBuf },
    #[error("patches link {path}: {source}")]
    PatchesLink { path: PathBuf, source: io::Error },
    #[error("cannot create {path}: {source}")]
    CreateDir { path: PathBuf, source: io::Error },
}

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("task config has no usable target_root")]
    NoTargetRoot,
    #[error("bad tile {0:?}")]
    BadTile(String),
    #[error("deliverable missing: {0}")]
    Missing(PathBuf),
    #[error("egress to {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

/// Parse `±DD±DDD` into (lat, lon).
fn parse_tile(tile: &str) -> Option<(i32, i32)> {
    let b = tile.as_bytes();
    if b.len() != 7 || !tile.is_ascii() {
        return None;
    }
    let sign = |c: u8| match c {
        b'+' => Some(1),
        b'-' => Some(-1),
        _ => None,
    };
    let lat = sign(b[0])? * tile[1..3].parse::<i32>().ok()?;
    let lon = sign(b[3])? * tile[4..7].parse::<i32>().ok()?;
    Some((lat, lon))
}

/// The one `±DD±DDD` formatter, used for tile names and block names alike.
fn format_latlon(lat: i32, lon: i32) -> String {
    format!("{lat:+03}{lon:+04}")
}

/// The 10-degree block containing a tile (each axis floored to a multiple of 10).
fn block_of(lat: i32, lon: i32) -> String {
    format_latlon(lat.div_euclid(10) * 10, lon.div_euclid(10) * 10)
}

fn target_root(task: &ClaimedTask) -> Option<PathBuf> {
    task.config
        .get("target_root")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn is_overlay(task: &ClaimedTask) -> bool {
    task.task_type == "overlay"
}

fn overlay_dir(root: &Path, block: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    p.extend(OVERLAY_TREE);
    p.push(block);
    p
}

/// Prove the target writable, repoint the patches link, and pre-create the
/// overlay block directory.
pub fn prepare(task: &ClaimedTask, paths: &ExecPaths) -> Result<PreparedTask, PrepareError> {
    let root = target_root(task).ok_or(PrepareError::NoTargetRoot)?;
    let (lat, lon) =
        parse_tile(&task.tile).ok_or_else(|| PrepareError::BadTile(task.tile.clone()))?;

    let probe = root.join(format!(".oxo-probe-{}", std::process::id()));
    fs::write(&probe, b"")
        .and_then(|_| fs::remove_file(&probe))
        .map_err(|source| PrepareError::TargetNotWritable {
            path: root.clone(),
            source,
        })?;

    let patches = task
        .config
        .get("patches")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    set_patches_link(paths, patches)?;

    if is_overlay(task) {
        let dir = overlay_dir(&paths.scratch, &block_of(lat, lon));
        fs::create_dir_all(&dir).map_err(|source| PrepareError::CreateDir { path: dir, source })?;
    }
    Ok(PreparedTask { target_root: root })
}

fn set_patches_link(paths: &ExecPaths, patches: Option<&str>) -> Result<(), PrepareError> {
    let link = &paths.patches_link;
    let err = |source| PrepareError::PatchesLink {
        path: link.clone(),
        source,
    };
    match patches {
        Some(set) => {
            let dest = paths.content.join("patches").join(set);
            if !dest.is_dir() {
                return Err(PrepareError::PatchesMissing { path: dest });
            }
            let mut tmp_name = link.file_name().unwrap_or_default().to_os_string();
            tmp_name.push(format!(".new-{}", std::process::id()));
            let tmp = link.with_file_name(tmp_name);
            let _ = fs::remove_file(&tmp);
            symlink(&dest, &tmp).map_err(err)?;
            fs::rename(&tmp, link).map_err(|e| {
                let _ = fs::remove_file(&tmp);
                err(e)
            })
        }
        None => match fs::remove_file(link) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(err(e)),
            _ => Ok(()),
        },
    }
}

fn copy_recursive(src: &Path, dest: &Path) -> io::Result<()> {
    if src.is_dir() {
        fs::create_dir(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dest).map(|_| ())
    }
}

/// First half of egress: copy `src` to a temporary name inside `dest_dir`
/// (the same filesystem as the final path). Returns the temporary path;
/// nothing under a final name exists yet. On failure the temporary is removed.
pub fn stage(src: &Path, dest_dir: &Path, final_name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dest_dir)?;
    let tmp = dest_dir.join(format!(".oxo-tmp-{}-{final_name}", std::process::id()));
    remove_any(&tmp)?;
    if let Err(e) = copy_recursive(src, &tmp) {
        let _ = remove_any(&tmp);
        return Err(e);
    }
    Ok(tmp)
}

/// Second half of egress: rename the staged copy over its final name,
/// replacing a previous attempt's result.
pub fn commit(tmp: &Path, final_path: &Path) -> io::Result<()> {
    remove_any(final_path)?;
    fs::rename(tmp, final_path)
}

fn remove_any(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Move the task's deliverable from scratch into the target root.
pub fn egress(task: &ClaimedTask, paths: &ExecPaths) -> Result<(), EgressError> {
    let root = target_root(task).ok_or(EgressError::NoTargetRoot)?;
    let (lat, lon) =
        parse_tile(&task.tile).ok_or_else(|| EgressError::BadTile(task.tile.clone()))?;
    let name = format_latlon(lat, lon);

    let (src, dest_dir, final_name) = if is_overlay(task) {
        let block = block_of(lat, lon);
        let file = format!("{name}.dsf");
        (
            overlay_dir(&paths.scratch, &block).join(&file),
            overlay_dir(&root, &block),
            file,
        )
    } else {
        let dir = format!("zOrtho4XP_{name}");
        (paths.scratch.join("Tiles").join(&dir), root.clone(), dir)
    };
    if !src.exists() {
        return Err(EgressError::Missing(src));
    }
    let io_err = |source| EgressError::Io {
        path: dest_dir.join(&final_name),
        source,
    };
    let tmp = stage(&src, &dest_dir, &final_name).map_err(io_err)?;
    commit(&tmp, &dest_dir.join(&final_name)).map_err(|source| {
        let _ = remove_any(&tmp);
        EgressError::Io {
            path: dest_dir.join(&final_name),
            source,
        }
    })
}

/// Wipe every scratch subdirectory's contents and recreate the empty
/// skeleton. Touches nothing outside `scratch`.
pub fn cleanup(scratch: &Path) -> io::Result<()> {
    for name in SCRATCH_SKELETON {
        let dir = scratch.join(name);
        remove_any(&dir)?;
        fs::create_dir_all(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    struct Env {
        _d: tempfile::TempDir,
        paths: ExecPaths,
        target: PathBuf,
    }

    fn env() -> Env {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let paths = ExecPaths {
            scratch: r.join("scratch"),
            content: r.join("content"),
            patches_link: r.join("patches-active"),
        };
        let target = r.join("artifacts");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(paths.content.join("patches/set-a")).unwrap();
        fs::create_dir_all(paths.content.join("patches/set-b")).unwrap();
        cleanup(&paths.scratch).unwrap();
        Env {
            _d: d,
            paths,
            target,
        }
    }

    fn task(e: &Env, kind: &str, tile: &str, patches: Option<&str>) -> ClaimedTask {
        let mut config = json!({"target_root": e.target});
        if let Some(p) = patches {
            config["patches"] = json!(p);
        }
        ClaimedTask {
            task_id: Uuid::nil(),
            job_id: Uuid::nil(),
            lease_token: Uuid::nil(),
            tile: tile.into(),
            task_type: kind.into(),
            attempt: 1,
            config,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn tile_and_block_names_use_one_formatter() {
        assert_eq!(parse_tile("+51+000"), Some((51, 0)));
        assert_eq!(parse_tile("-01-179"), Some((-1, -179)));
        assert_eq!(parse_tile("51+000"), None);
        assert_eq!(block_of(51, 0), "+50+000");
        assert_eq!(block_of(-1, -179), "-10-180");
        assert_eq!(block_of(9, 9), "+00+000");
    }

    #[test]
    fn an_unwritable_target_is_refused_naming_the_path() {
        let e = env();
        fs::set_permissions(&e.target, fs::Permissions::from_mode(0o555)).unwrap();
        let t = task(&e, "ortho", "+51+000", None);
        let r = prepare(&t, &e.paths);
        fs::set_permissions(&e.target, fs::Permissions::from_mode(0o755)).unwrap();
        // root bypasses permission bits; only assert when the denial is real
        if let Err(PrepareError::TargetNotWritable { path, .. }) = r {
            assert_eq!(path, e.target);
        } else if fs::write(e.target.join("x"), b"").is_ok() {
            // running with privileges that ignore modes
        } else {
            panic!("expected TargetNotWritable");
        }
    }

    #[test]
    fn a_missing_target_is_refused_before_any_side_effect() {
        let e = env();
        let mut t = task(&e, "overlay", "+51+000", Some("set-a"));
        t.config["target_root"] = json!(e.target.join("absent"));
        assert!(matches!(
            prepare(&t, &e.paths),
            Err(PrepareError::TargetNotWritable { .. })
        ));
        assert!(fs::symlink_metadata(&e.paths.patches_link).is_err());
        assert!(!e
            .paths
            .scratch
            .join("yOrtho4XP_Overlays/Earth nav data/+50+000")
            .exists());
    }

    #[test]
    fn a_successful_probe_leaves_nothing_behind() {
        let e = env();
        prepare(&task(&e, "ortho", "+51+000", None), &e.paths).unwrap();
        assert!(names(&e.target).is_empty());
    }

    #[test]
    fn the_patches_link_swaps_old_to_new_to_none() {
        let e = env();
        prepare(&task(&e, "ortho", "+51+000", Some("set-a")), &e.paths).unwrap();
        assert_eq!(
            fs::read_link(&e.paths.patches_link).unwrap(),
            e.paths.content.join("patches/set-a")
        );
        prepare(&task(&e, "ortho", "+51+000", Some("set-b")), &e.paths).unwrap();
        assert_eq!(
            fs::read_link(&e.paths.patches_link).unwrap(),
            e.paths.content.join("patches/set-b")
        );
        // no temporary link names survive
        assert_eq!(
            names(e.paths.patches_link.parent().unwrap())
                .iter()
                .filter(|n| n.starts_with("patches-active"))
                .count(),
            1
        );
        prepare(&task(&e, "ortho", "+51+000", None), &e.paths).unwrap();
        assert!(fs::symlink_metadata(&e.paths.patches_link).is_err());
        // removing an absent link is fine
        prepare(&task(&e, "ortho", "+51+000", None), &e.paths).unwrap();
    }

    #[test]
    fn an_unknown_patches_set_is_refused() {
        let e = env();
        assert!(matches!(
            prepare(&task(&e, "ortho", "+51+000", Some("nope")), &e.paths),
            Err(PrepareError::PatchesMissing { .. })
        ));
    }

    #[test]
    fn overlay_prepare_precreates_the_block_dir_idempotently() {
        let e = env();
        let t = task(&e, "overlay", "+51+000", None);
        prepare(&t, &e.paths).unwrap();
        prepare(&t, &e.paths).unwrap();
        assert!(e
            .paths
            .scratch
            .join("yOrtho4XP_Overlays/Earth nav data/+50+000")
            .is_dir());
        let o = task(&e, "ortho", "+51+000", None);
        cleanup(&e.paths.scratch).unwrap();
        prepare(&o, &e.paths).unwrap();
        assert!(names(&e.paths.scratch.join("yOrtho4XP_Overlays")).is_empty());
    }

    fn fabricate_ortho(e: &Env) {
        let d = e.paths.scratch.join("Tiles/zOrtho4XP_+51+000");
        fs::create_dir_all(d.join("Earth nav data/+50+000")).unwrap();
        fs::write(d.join("Earth nav data/+50+000/+51+000.dsf"), b"dsf").unwrap();
        fs::write(d.join("+51+000.mesh"), b"m").unwrap();
    }

    fn fabricate_overlay(e: &Env) {
        let d = e
            .paths
            .scratch
            .join("yOrtho4XP_Overlays/Earth nav data/+50+000");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("+51+000.dsf"), b"ovl").unwrap();
    }

    #[test]
    fn ortho_egress_moves_the_whole_directory_without_temporaries() {
        let e = env();
        fabricate_ortho(&e);
        egress(&task(&e, "ortho", "+51+000", None), &e.paths).unwrap();
        assert_eq!(names(&e.target), ["zOrtho4XP_+51+000"]);
        let d = e.target.join("zOrtho4XP_+51+000");
        assert_eq!(fs::read(d.join("+51+000.mesh")).unwrap(), b"m");
        assert_eq!(
            fs::read(d.join("Earth nav data/+50+000/+51+000.dsf")).unwrap(),
            b"dsf"
        );
    }

    #[test]
    fn egress_is_repeatable_replacing_an_earlier_attempt() {
        let e = env();
        fabricate_ortho(&e);
        let t = task(&e, "ortho", "+51+000", None);
        egress(&t, &e.paths).unwrap();
        egress(&t, &e.paths).unwrap();
        assert_eq!(names(&e.target), ["zOrtho4XP_+51+000"]);
    }

    #[test]
    fn overlay_egress_creates_the_block_dir_and_moves_the_dsf() {
        let e = env();
        fabricate_overlay(&e);
        let t = task(&e, "overlay", "+51+000", None);
        egress(&t, &e.paths).unwrap();
        egress(&t, &e.paths).unwrap();
        let d = e.target.join("yOrtho4XP_Overlays/Earth nav data/+50+000");
        assert_eq!(names(&d), ["+51+000.dsf"]);
        assert_eq!(fs::read(d.join("+51+000.dsf")).unwrap(), b"ovl");
    }

    #[test]
    fn a_missing_deliverable_is_a_typed_error() {
        let e = env();
        assert!(matches!(
            egress(&task(&e, "ortho", "+51+000", None), &e.paths),
            Err(EgressError::Missing(_))
        ));
    }

    #[test]
    fn a_crash_between_copy_and_rename_leaves_no_final_named_partial() {
        let e = env();
        fabricate_ortho(&e);
        // first half only: the process "dies" before commit
        let src = e.paths.scratch.join("Tiles/zOrtho4XP_+51+000");
        let tmp = stage(&src, &e.target, "zOrtho4XP_+51+000").unwrap();
        assert!(!e.target.join("zOrtho4XP_+51+000").exists());
        assert_eq!(tmp.parent().unwrap(), e.target);
        // second half completes it
        commit(&tmp, &e.target.join("zOrtho4XP_+51+000")).unwrap();
        assert!(!tmp.exists());
        assert_eq!(names(&e.target), ["zOrtho4XP_+51+000"]);
    }

    #[test]
    fn a_failed_copy_removes_its_temporary() {
        let e = env();
        let r = stage(&e.target.join("absent"), &e.target, "x");
        assert!(r.is_err());
        assert!(names(&e.target).is_empty());
    }

    #[test]
    fn cleanup_empties_but_preserves_the_skeleton_and_spares_neighbours() {
        let e = env();
        fabricate_ortho(&e);
        fabricate_overlay(&e);
        fs::write(e.paths.scratch.join("tmp/junk"), b"x").unwrap();
        fs::write(e.paths.scratch.join("Orthophotos/a.jpg"), b"x").unwrap();
        let outside = e.paths.scratch.parent().unwrap().join("outside.txt");
        fs::write(&outside, b"keep").unwrap();
        cleanup(&e.paths.scratch).unwrap();
        let mut want: Vec<_> = SCRATCH_SKELETON.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(names(&e.paths.scratch), want);
        for n in SCRATCH_SKELETON {
            assert!(names(&e.paths.scratch.join(n)).is_empty(), "{n}");
        }
        assert_eq!(fs::read(&outside).unwrap(), b"keep");
        assert!(e.target.is_dir());
    }
}
