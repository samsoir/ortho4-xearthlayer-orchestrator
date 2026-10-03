//! Filesystem lifecycle around one build: `prepare` before it, `egress`
//! after it, `cleanup` between tasks. Pure filesystem logic; every path is
//! injected through [`ExecPaths`].

use std::fs;
use std::io;
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
    #[error(
        "hollow deliverable for tile {tile}: the staged tree has no .dsf under \"Earth nav data/\""
    )]
    HollowDeliverable { tile: String },
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

/// Prove the target writable and pre-create the overlay block directory.
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

    if is_overlay(task) {
        let dir = overlay_dir(&paths.scratch, &block_of(lat, lon));
        fs::create_dir_all(&dir).map_err(|source| PrepareError::CreateDir { path: dir, source })?;
    }
    Ok(PreparedTask { target_root: root })
}

/// Extensions that never ship, wherever they sit (the XEL deliverable carries
/// no imagery; the consumer streams it).
const NEVER_SHIPPED: [&str; 3] = ["jpg", "jpeg", "dds"];

fn never_shipped(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| NEVER_SHIPPED.iter().any(|n| e.eq_ignore_ascii_case(n)))
}

/// Does a top-level entry of the tile directory belong to the ship-set?
/// `Earth nav data/**` (DSF), `terrain/**` (`.ter`) and `textures/*.png`
/// (the water masks). Everything else perishes with scratch.
fn ships(rel: &Path, is_dir: bool) -> bool {
    let mut comps = rel
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or(""));
    match (comps.next(), comps.next()) {
        (Some("Earth nav data" | "terrain"), _) => !never_shipped(rel),
        (Some("textures"), None) => is_dir,
        (Some("textures"), Some(_)) => {
            !is_dir
                && rel
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        }
        _ => false,
    }
}

fn copy_recursive(src: &Path, dest: &Path, rel_dir: &Path) -> io::Result<()> {
    if src.is_dir() {
        fs::create_dir(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            let rel = rel_dir.join(&name);
            let path = entry.path();
            if !ships(&rel, path.is_dir()) {
                continue;
            }
            copy_recursive(&path, &dest.join(name), &rel)?;
        }
        Ok(())
    } else {
        fs::copy(src, dest).map(|_| ())
    }
}

/// First half of egress: copy `src` to a temporary name inside `dest_dir`
/// (the same filesystem as the final path). A directory `src` is filtered to
/// the ship-set; a file is copied as is. Returns the temporary path;
/// nothing under a final name exists yet. On failure the temporary is removed.
pub fn stage(src: &Path, dest_dir: &Path, final_name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dest_dir)?;
    let tmp = dest_dir.join(format!(".oxo-tmp-{}-{final_name}", std::process::id()));
    remove_any(&tmp)?;
    if let Err(e) = copy_recursive(src, &tmp, Path::new("")) {
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

/// Whether `dir` contains a `.dsf` file at any depth.
fn has_dsf(dir: &Path) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => has_dsf(&p),
            Ok(t) if t.is_file() => p.extension().is_some_and(|x| x == "dsf"),
            _ => false,
        }
    })
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
    // A tile always has exactly one DSF; without one the staged tree is
    // hollow and must never replace a previous good delivery.
    if !is_overlay(task) && !has_dsf(&tmp.join("Earth nav data")) {
        let _ = remove_any(&tmp);
        return Err(EgressError::HollowDeliverable {
            tile: task.tile.clone(),
        });
    }
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
    use std::os::unix::fs::{symlink, PermissionsExt};

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
        };
        let target = r.join("artifacts");
        fs::create_dir_all(&target).unwrap();
        cleanup(&paths.scratch).unwrap();
        Env {
            _d: d,
            paths,
            target,
        }
    }

    fn task(e: &Env, kind: &str, tile: &str) -> ClaimedTask {
        let config = json!({"target_root": e.target});
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

    fn is_root() -> bool {
        fs::read_to_string("/proc/self/status")
            .map(|t| {
                t.lines()
                    .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(2) == Some("0"))
            })
            .unwrap_or(false)
    }

    #[test]
    fn an_unwritable_target_is_refused_naming_the_path() {
        if is_root() {
            return; // modes are ignored for root
        }
        let e = env();
        fs::set_permissions(&e.target, fs::Permissions::from_mode(0o555)).unwrap();
        let r = prepare(&task(&e, "ortho", "+51+000"), &e.paths);
        fs::set_permissions(&e.target, fs::Permissions::from_mode(0o755)).unwrap();
        match r {
            Err(PrepareError::TargetNotWritable { path, .. }) => assert_eq!(path, e.target),
            other => panic!("expected TargetNotWritable, got {other:?}"),
        }
    }

    #[test]
    fn a_target_under_a_regular_file_is_refused_even_as_root() {
        let e = env();
        let file = e.target.join("file");
        fs::write(&file, b"").unwrap();
        let mut t = task(&e, "ortho", "+51+000");
        t.config["target_root"] = json!(file.join("sub"));
        assert!(matches!(
            prepare(&t, &e.paths),
            Err(PrepareError::TargetNotWritable { .. })
        ));
    }

    #[test]
    fn a_missing_target_is_refused_before_any_side_effect() {
        let e = env();
        let mut t = task(&e, "overlay", "+51+000");
        t.config["target_root"] = json!(e.target.join("absent"));
        assert!(matches!(
            prepare(&t, &e.paths),
            Err(PrepareError::TargetNotWritable { .. })
        ));
        assert!(!e
            .paths
            .scratch
            .join("yOrtho4XP_Overlays/Earth nav data/+50+000")
            .exists());
    }

    #[test]
    fn a_successful_probe_leaves_nothing_behind() {
        let e = env();
        prepare(&task(&e, "ortho", "+51+000"), &e.paths).unwrap();
        assert!(names(&e.target).is_empty());
    }

    #[test]
    fn overlay_prepare_precreates_the_block_dir_idempotently() {
        let e = env();
        let t = task(&e, "overlay", "+51+000");
        prepare(&t, &e.paths).unwrap();
        prepare(&t, &e.paths).unwrap();
        assert!(e
            .paths
            .scratch
            .join("yOrtho4XP_Overlays/Earth nav data/+50+000")
            .is_dir());
        let o = task(&e, "ortho", "+51+000");
        cleanup(&e.paths.scratch).unwrap();
        prepare(&o, &e.paths).unwrap();
        assert!(names(&e.paths.scratch.join("yOrtho4XP_Overlays")).is_empty());
    }

    fn fabricate_ortho(e: &Env) {
        let d = e.paths.scratch.join("Tiles/zOrtho4XP_+51+000");
        fs::create_dir_all(d.join("Earth nav data/+50+000")).unwrap();
        fs::create_dir_all(d.join("terrain")).unwrap();
        fs::create_dir_all(d.join("textures")).unwrap();
        // the ship-set
        fs::write(d.join("Earth nav data/+50+000/+51+000.dsf"), b"dsf").unwrap();
        fs::write(d.join("terrain/water_1.ter"), b"ter").unwrap();
        fs::write(d.join("textures/mask_1.png"), b"png").unwrap();
        // contaminants
        fs::write(d.join("textures/foo.jpg"), b"x").unwrap();
        fs::write(d.join("textures/bar.dds"), b"x").unwrap();
        fs::write(d.join("Data+51+000.mesh"), b"m").unwrap();
        fs::write(d.join("Ortho4XP_+51+000.cfg"), b"c").unwrap();
        fs::write(d.join("Ortho4XP_+51+000.cfg.bak"), b"c").unwrap();
    }

    fn fabricate_overlay(e: &Env) {
        let d = e
            .paths
            .scratch
            .join("yOrtho4XP_Overlays/Earth nav data/+50+000");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("+51+000.dsf"), b"ovl").unwrap();
    }

    fn no_temporaries(dir: &Path) {
        assert!(
            names(dir).iter().all(|n| !n.starts_with(".oxo-tmp-")),
            "{:?}",
            names(dir)
        );
    }

    #[test]
    fn ortho_egress_ships_the_xel_tile_and_keeps_the_source() {
        let e = env();
        fabricate_ortho(&e);
        egress(&task(&e, "ortho", "+51+000"), &e.paths).unwrap();
        assert_eq!(names(&e.target), ["zOrtho4XP_+51+000"]);
        let d = e.target.join("zOrtho4XP_+51+000");
        assert_eq!(
            fs::read(d.join("Earth nav data/+50+000/+51+000.dsf")).unwrap(),
            b"dsf"
        );
        assert_eq!(fs::read(d.join("terrain/water_1.ter")).unwrap(), b"ter");
        assert_eq!(fs::read(d.join("textures/mask_1.png")).unwrap(), b"png");
        let src = e.paths.scratch.join("Tiles/zOrtho4XP_+51+000");
        assert_eq!(fs::read(src.join("Data+51+000.mesh")).unwrap(), b"m");
    }

    #[test]
    fn ortho_egress_withholds_jpegs() {
        let e = env();
        fabricate_ortho(&e);
        egress(&task(&e, "ortho", "+51+000"), &e.paths).unwrap();
        assert!(!e.target.join("zOrtho4XP_+51+000/textures/foo.jpg").exists());
    }

    #[test]
    fn ortho_egress_withholds_dds() {
        let e = env();
        fabricate_ortho(&e);
        egress(&task(&e, "ortho", "+51+000"), &e.paths).unwrap();
        assert!(!e.target.join("zOrtho4XP_+51+000/textures/bar.dds").exists());
    }

    #[test]
    fn ortho_egress_withholds_mesh_intermediates() {
        let e = env();
        fabricate_ortho(&e);
        egress(&task(&e, "ortho", "+51+000"), &e.paths).unwrap();
        assert!(!e.target.join("zOrtho4XP_+51+000/Data+51+000.mesh").exists());
    }

    #[test]
    fn ortho_egress_withholds_the_tile_cfg() {
        let e = env();
        fabricate_ortho(&e);
        egress(&task(&e, "ortho", "+51+000"), &e.paths).unwrap();
        let d = e.target.join("zOrtho4XP_+51+000");
        assert!(!d.join("Ortho4XP_+51+000.cfg").exists());
        assert!(!d.join("Ortho4XP_+51+000.cfg.bak").exists());
    }

    #[test]
    fn ortho_egress_replaces_a_stale_final_wholesale() {
        let e = env();
        fabricate_ortho(&e);
        let stale = e.target.join("zOrtho4XP_+51+000");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("stale.txt"), b"old").unwrap();
        fs::create_dir_all(stale.join("terrain")).unwrap();
        fs::write(stale.join("terrain/water_1.ter"), b"old").unwrap();
        egress(&task(&e, "ortho", "+51+000"), &e.paths).unwrap();
        assert!(!stale.join("stale.txt").exists());
        assert_eq!(fs::read(stale.join("terrain/water_1.ter")).unwrap(), b"ter");
        no_temporaries(&e.target);
    }

    #[test]
    fn overlay_egress_replaces_a_stale_dsf_and_keeps_the_source() {
        let e = env();
        fabricate_overlay(&e);
        let d = e.target.join("yOrtho4XP_Overlays/Earth nav data/+50+000");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("+51+000.dsf"), b"old").unwrap();
        egress(&task(&e, "overlay", "+51+000"), &e.paths).unwrap();
        assert_eq!(names(&d), ["+51+000.dsf"]);
        assert_eq!(fs::read(d.join("+51+000.dsf")).unwrap(), b"ovl");
        let src = e
            .paths
            .scratch
            .join("yOrtho4XP_Overlays/Earth nav data/+50+000/+51+000.dsf");
        assert_eq!(fs::read(src).unwrap(), b"ovl");
    }

    #[test]
    fn a_missing_deliverable_is_a_typed_error() {
        let e = env();
        assert!(matches!(
            egress(&task(&e, "ortho", "+51+000"), &e.paths),
            Err(EgressError::Missing(_))
        ));
    }

    fn hollow_ortho(e: &Env) {
        fabricate_ortho(e);
        let d = e.paths.scratch.join("Tiles/zOrtho4XP_+51+000");
        fs::remove_file(d.join("Earth nav data/+50+000/+51+000.dsf")).unwrap();
    }

    #[test]
    fn an_ortho_tile_without_a_dsf_is_refused_and_commits_nothing() {
        let e = env();
        hollow_ortho(&e);
        let r = egress(&task(&e, "ortho", "+51+000"), &e.paths);
        match r {
            Err(EgressError::HollowDeliverable { tile }) => assert_eq!(tile, "+51+000"),
            other => panic!("expected HollowDeliverable, got {other:?}"),
        }
        assert!(names(&e.target).is_empty(), "{:?}", names(&e.target));
        no_temporaries(&e.target);
    }

    #[test]
    fn a_hollow_rerun_leaves_a_previous_good_delivery_intact() {
        let e = env();
        let good = e.target.join("zOrtho4XP_+51+000");
        fs::create_dir_all(good.join("Earth nav data/+50+000")).unwrap();
        fs::write(good.join("Earth nav data/+50+000/+51+000.dsf"), b"good").unwrap();
        hollow_ortho(&e);
        let r = egress(&task(&e, "ortho", "+51+000"), &e.paths);
        assert!(matches!(r, Err(EgressError::HollowDeliverable { .. })));
        assert_eq!(
            fs::read(good.join("Earth nav data/+50+000/+51+000.dsf")).unwrap(),
            b"good"
        );
        assert_eq!(names(&e.target), ["zOrtho4XP_+51+000"]);
        no_temporaries(&e.target);
    }

    #[test]
    fn a_crash_between_copy_and_rename_leaves_no_final_named_partial() {
        let e = env();
        fabricate_ortho(&e);
        let src = e.paths.scratch.join("Tiles/zOrtho4XP_+51+000");
        let tmp = stage(&src, &e.target, "zOrtho4XP_+51+000").unwrap();
        assert!(!e.target.join("zOrtho4XP_+51+000").exists());
        assert_eq!(tmp.parent().unwrap(), e.target);
        assert!(
            src.join("terrain/water_1.ter").exists(),
            "stage must copy, not move"
        );
        assert_eq!(
            fs::read(tmp.join("terrain/water_1.ter")).unwrap(),
            b"ter",
            "stage applies the filter"
        );
        assert!(!tmp.join("Data+51+000.mesh").exists());
        commit(&tmp, &e.target.join("zOrtho4XP_+51+000")).unwrap();
        assert!(!tmp.exists());
        assert_eq!(names(&e.target), ["zOrtho4XP_+51+000"]);
    }

    #[test]
    fn a_copy_failing_partway_removes_its_temporary_and_publishes_nothing() {
        let e = env();
        fabricate_ortho(&e);
        let src = e.paths.scratch.join("Tiles/zOrtho4XP_+51+000");
        symlink("/nonexistent/oxo-dangling", src.join("terrain/zz-dangling")).unwrap();
        assert!(stage(&src, &e.target, "zOrtho4XP_+51+000").is_err());
        no_temporaries(&e.target);
        let r = egress(&task(&e, "ortho", "+51+000"), &e.paths);
        assert!(matches!(r, Err(EgressError::Io { .. })));
        assert!(names(&e.target).is_empty());
    }

    #[test]
    fn a_failed_commit_cleans_its_temporary_under_the_target_root() {
        if is_root() {
            return; // cannot make a removal fail as root
        }
        let e = env();
        fabricate_ortho(&e);
        let stale = e.target.join("zOrtho4XP_+51+000");
        fs::create_dir_all(stale.join("locked")).unwrap();
        fs::write(stale.join("locked/f"), b"x").unwrap();
        fs::set_permissions(stale.join("locked"), fs::Permissions::from_mode(0o555)).unwrap();
        let r = egress(&task(&e, "ortho", "+51+000"), &e.paths);
        fs::set_permissions(stale.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(r, Err(EgressError::Io { .. })));
        no_temporaries(&e.target);
        let src = e.paths.scratch.join("Tiles/zOrtho4XP_+51+000");
        assert!(src.join("terrain/water_1.ter").exists());
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
