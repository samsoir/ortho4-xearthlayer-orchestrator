//! The real runner (`worker/oxo_o4_runner.py`) against a fake `O4_*` tree.
//! Needs `python3` on PATH; skips with a message when absent.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn have_python() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

struct Out {
    code: Option<i32>,
    stdout: String,
    log: String,
}

fn run(task_type: &str, tile: &str, envs: &[(&str, &str)]) -> Option<Out> {
    if !have_python() {
        eprintln!("SKIP: python3 not found; the runner contract tests need it");
        return None;
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runner = manifest.join("../worker/oxo_o4_runner.py");
    let fake = manifest.join("tests/fixtures/fake_o4");
    let work = tempfile::tempdir().unwrap();
    // The fake tree is read in place; scratch writes go to a copy.
    let install = work.path().join("install");
    copy_dir(&fake, &install);
    let log = work.path().join("calls.log");
    let input = serde_json::json!({
        "tile": tile, "task_type": task_type,
        "config": {"v":1,"provider":"BI","zoom":16,"raw":{"cover_zl":14,"max_convergence_angle":2},"target_root":"/x"},
        "install_root": install, "overlay_src": "/xp/Global Scenery",
    });
    let mut cmd = Command::new("python3");
    cmd.arg(&runner)
        .env("FAKE_O4_LOG", &log)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    Some(Out {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        log: std::fs::read_to_string(&log).unwrap_or_default(),
    })
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn last_json(stdout: &str) -> serde_json::Value {
    let line = stdout.lines().rfind(|l| !l.trim().is_empty()).unwrap();
    serde_json::from_str(line).unwrap_or_else(|_| panic!("last line not JSON: {stdout:?}"))
}

fn calls(log: &str) -> Vec<&str> {
    log.lines()
        .filter_map(|l| l.strip_prefix("call "))
        .collect()
}

#[test]
fn ortho_success_runs_the_four_builds_in_order() {
    let Some(o) = run("ortho", "+50-002", &[]) else {
        return;
    };
    assert_eq!(o.code, Some(0), "{}", o.stdout);
    assert_eq!(last_json(&o.stdout)["outcome"], "ok");
    assert_eq!(
        calls(&o.log),
        ["build_poly_file", "build_mesh", "build_masks", "build_tile"]
    );
}

#[test]
fn imagery_dictionaries_are_initialised_before_any_build() {
    let Some(o) = run("ortho", "+50-002", &[]) else {
        return;
    };
    let lines: Vec<&str> = o.log.lines().collect();
    let img: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.starts_with("img "))
        .collect();
    assert_eq!(
        img,
        [
            "img initialize_extents_dict",
            "img initialize_color_filters_dict",
            "img initialize_providers_dict",
            "img initialize_combined_providers_dict"
        ]
    );
    let first_build = lines.iter().position(|l| l.starts_with("call ")).unwrap();
    let last_img = lines.iter().rposition(|l| l.starts_with("img ")).unwrap();
    assert!(last_img < first_build);
}

#[test]
fn exception_in_build_mesh_names_phase_and_text() {
    let Some(o) = run("ortho", "+50-002", &[("FAKE_O4_FAIL", "build_mesh")]) else {
        return;
    };
    assert_eq!(o.code, Some(1));
    let j = last_json(&o.stdout);
    assert_eq!(j["outcome"], "failed");
    assert_eq!(j["phase"], "build_mesh");
    assert!(j["reason"]
        .as_str()
        .unwrap()
        .contains("RuntimeError: boom in build_mesh"));
    assert_eq!(calls(&o.log), ["build_poly_file", "build_mesh"]);
}

#[test]
fn config_is_applied_to_the_tile() {
    let Some(o) = run("ortho", "-33+151", &[]) else {
        return;
    };
    assert!(o.log.contains("tile -33 151 ''"), "{}", o.log);
    let attrs = o.log.lines().find(|l| l.starts_with("tileattrs ")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&attrs["tileattrs ".len()..]).unwrap();
    assert_eq!(v["default_website"], "BI");
    assert_eq!(v["default_zl"], 16);
    assert_eq!(v["cover_zl"], 14);
    assert_eq!(v["max_convergence_angle"], 2);
}

#[test]
fn custom_overlay_src_is_set_for_both_task_types() {
    for t in ["ortho", "overlay"] {
        let Some(o) = run(t, "+50-002", &[]) else {
            return;
        };
        assert_eq!(o.code, Some(0), "{t}: {}", o.stdout);
        assert!(
            o.log.contains("cfg overlay_src=/xp/Global Scenery"),
            "{t}: {}",
            o.log
        );
    }
}

#[test]
fn overlay_precreates_block_dir_then_builds_only_the_overlay() {
    let Some(o) = run("overlay", "+50-002", &[]) else {
        return;
    };
    assert_eq!(o.code, Some(0), "{}", o.stdout);
    assert_eq!(calls(&o.log), ["build_overlay"]);
    assert!(o.log.contains("blockdir_exists True"), "{}", o.log);
    assert!(o.log.contains("args 50 -2"));
}

#[test]
fn stdout_noise_from_o4_cannot_pollute_the_result_line() {
    let Some(o) = run("ortho", "+50-002", &[("FAKE_O4_NOISE", "1")]) else {
        return;
    };
    assert_eq!(o.code, Some(0));
    assert!(!o.stdout.contains("noise"), "{}", o.stdout);
    assert_eq!(last_json(&o.stdout)["outcome"], "ok");
}

#[test]
fn bad_tile_is_a_failed_line_not_a_traceback() {
    let Some(o) = run("ortho", "garbage", &[]) else {
        return;
    };
    assert_eq!(o.code, Some(1));
    assert_eq!(last_json(&o.stdout)["outcome"], "failed");
}

#[test]
fn subprocess_chatter_on_inherited_fd1_cannot_pollute_the_result() {
    let Some(o) = run("ortho", "+50-002", &[("FAKE_O4_NOISE", "1")]) else {
        return;
    };
    assert_eq!(o.code, Some(0));
    assert!(!o.stdout.contains("triangle chatter"), "{}", o.stdout);
    assert_eq!(o.stdout.lines().count(), 1, "{}", o.stdout);
    assert_eq!(last_json(&o.stdout)["outcome"], "ok");
}

#[test]
fn provider_initialisation_failure_names_its_phase() {
    let Some(o) = run(
        "ortho",
        "+50-002",
        &[("FAKE_O4_FAIL", "initialize_providers_dict")],
    ) else {
        return;
    };
    assert_eq!(o.code, Some(1));
    let j = last_json(&o.stdout);
    assert_eq!(j["outcome"], "failed");
    assert_eq!(j["phase"], "initialize_providers");
    assert!(j["reason"]
        .as_str()
        .unwrap()
        .contains("boom in initialize_providers_dict"));
    assert!(calls(&o.log).is_empty());
}
