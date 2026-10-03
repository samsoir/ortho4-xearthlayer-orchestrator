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
    stderr: String,
    log: String,
}

/// The knobs a test turns beyond the defaults.
struct Case {
    raw: serde_json::Value,
    skip_converts: bool,
    app_overrides: serde_json::Value,
    /// Replace the wire value of `skip_converts`; `Some(Null)` omits the key
    /// (the stale v1 payload shape).
    skip_converts_wire: Option<serde_json::Value>,
    /// Create `Patches/<long_latlon>` in the install before running.
    patches_dir: Option<&'static str>,
}

impl Default for Case {
    fn default() -> Self {
        Case {
            raw: serde_json::json!({"cover_zl":"14","clean_bad_geometries":"False","sea_texture_blur":"0.5","zone_list_like":"[1, 2]"}),
            skip_converts: true,
            app_overrides: serde_json::json!({}),
            skip_converts_wire: None,
            patches_dir: None,
        }
    }
}

fn run(task_type: &str, tile: &str, envs: &[(&str, &str)]) -> Option<Out> {
    // Raw values are strings on the wire, exactly as plan() emits them.
    run_case(task_type, tile, envs, Case::default())
}

fn run_raw(
    task_type: &str,
    tile: &str,
    envs: &[(&str, &str)],
    raw: serde_json::Value,
) -> Option<Out> {
    run_case(
        task_type,
        tile,
        envs,
        Case {
            raw,
            ..Case::default()
        },
    )
}

fn run_case(task_type: &str, tile: &str, envs: &[(&str, &str)], case: Case) -> Option<Out> {
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
    if let Some(rel) = case.patches_dir {
        std::fs::create_dir_all(install.join("Patches").join(rel)).unwrap();
    }
    let log = work.path().join("calls.log");
    let mut config = serde_json::json!({"v":2,"provider":"BI","zoom":16,"raw":case.raw,"target_root":"/x","skip_converts":case.skip_converts});
    match case.skip_converts_wire {
        Some(serde_json::Value::Null) => {
            config.as_object_mut().unwrap().remove("skip_converts");
        }
        Some(v) => config["skip_converts"] = v,
        None => {}
    }
    let input = serde_json::json!({
        "tile": tile, "task_type": task_type,
        "config": config,
        "install_root": install, "overlay_src": "/xp/Global Scenery",
        "app_overrides": case.app_overrides,
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
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
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
    // Converted per cfg_vars[k]["type"], not left as wire strings.
    assert_eq!(v["cover_zl"], 14);
    // The silent-inversion hazard: bool("False") is True.
    assert_eq!(v["clean_bad_geometries"], false);
    assert_eq!(v["sea_texture_blur"], 0.5);
    assert_eq!(v["zone_list_like"], serde_json::json!([1, 2]));
}

#[test]
fn unknown_raw_key_fails_loudly_in_configure() {
    let Some(o) = run_raw(
        "ortho",
        "+50-002",
        &[],
        serde_json::json!({"no_such_variable":"1"}),
    ) else {
        return;
    };
    assert_eq!(o.code, Some(1));
    let j = last_json(&o.stdout);
    assert_eq!(j["outcome"], "failed");
    assert_eq!(j["phase"], "configure");
    assert!(j["reason"].as_str().unwrap().contains("no_such_variable"));
    assert!(calls(&o.log).is_empty());
}

#[test]
fn falsy_return_from_build_mesh_fails_and_stops() {
    let Some(o) = run("ortho", "+50-002", &[("FAKE_O4_RETURN_ZERO", "build_mesh")]) else {
        return;
    };
    assert_eq!(o.code, Some(1), "{}", o.stdout);
    let j = last_json(&o.stdout);
    assert_eq!(j["outcome"], "failed");
    assert_eq!(j["phase"], "build_mesh");
    assert_eq!(j["reason"], "build_mesh returned 0");
    assert_eq!(calls(&o.log), ["build_poly_file", "build_mesh"]);
}

#[test]
fn build_masks_zero_is_failure_but_none_is_success() {
    // Success (the default fake) returns None, as the real build_masks does.
    let Some(ok) = run("ortho", "+50-002", &[]) else {
        return;
    };
    assert_eq!(ok.code, Some(0), "{}", ok.stdout);
    let o = run(
        "ortho",
        "+50-002",
        &[("FAKE_O4_RETURN_ZERO", "build_masks")],
    )
    .unwrap();
    assert_eq!(o.code, Some(1), "{}", o.stdout);
    let j = last_json(&o.stdout);
    assert_eq!(j["phase"], "build_masks");
    assert_eq!(j["reason"], "build_masks returned 0");
    assert_eq!(
        calls(&o.log),
        ["build_poly_file", "build_mesh", "build_masks"]
    );
}

#[test]
fn falsy_return_from_build_overlay_fails() {
    let Some(o) = run(
        "overlay",
        "+50-002",
        &[("FAKE_O4_RETURN_ZERO", "build_overlay")],
    ) else {
        return;
    };
    assert_eq!(o.code, Some(1), "{}", o.stdout);
    let j = last_json(&o.stdout);
    assert_eq!(j["outcome"], "failed");
    assert_eq!(j["phase"], "build_overlay");
    assert_eq!(j["reason"], "build_overlay returned 0");
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

/// What one fake module holds of the app-level variables, from the log.
fn app_on(log: &str, module: &str) -> serde_json::Value {
    let prefix = format!("app {module} ");
    let line = log
        .lines()
        .rfind(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no app record for {module}: {log}"));
    serde_json::from_str(&line[prefix.len()..]).unwrap()
}

fn tile_attrs(log: &str) -> serde_json::Value {
    let attrs = log.lines().find(|l| l.starts_with("tileattrs ")).unwrap();
    serde_json::from_str(&attrs["tileattrs ".len()..]).unwrap()
}

fn assert_configure_failure(o: &Out, needle: &str) {
    assert_eq!(o.code, Some(1), "{}", o.stdout);
    let j = last_json(&o.stdout);
    assert_eq!(j["outcome"], "failed");
    assert_eq!(j["phase"], "configure");
    assert!(j["reason"].as_str().unwrap().contains(needle), "{j}");
    assert!(calls(&o.log).is_empty(), "{}", o.log);
}

#[test]
fn skip_converts_true_reaches_its_owning_module_for_both_task_types() {
    for t in ["ortho", "overlay"] {
        let Some(o) = run(t, "+50-002", &[]) else {
            return;
        };
        assert_eq!(o.code, Some(0), "{t}: {}", o.stdout);
        assert_eq!(
            app_on(&o.log, "O4_Tile_Utils")["skip_converts"],
            true,
            "{t}"
        );
        // Not on the wrong module.
        assert!(app_on(&o.log, "O4_Overlay_Utils")
            .get("skip_converts")
            .is_none());
    }
}

#[test]
fn an_explicit_false_skip_converts_reaches_the_owning_module() {
    let Some(o) = run_case(
        "ortho",
        "+50-002",
        &[],
        Case {
            skip_converts: false,
            ..Case::default()
        },
    ) else {
        return;
    };
    assert_eq!(o.code, Some(0), "{}", o.stdout);
    assert_eq!(app_on(&o.log, "O4_Tile_Utils")["skip_converts"], false);
    assert!(tile_attrs(&o.log).get("skip_converts").is_none());
}

#[test]
fn an_app_override_lands_typed_on_its_own_module_not_the_tile() {
    let Some(o) = run_case(
        "ortho",
        "+50-002",
        &[],
        Case {
            app_overrides: serde_json::json!({
                "max_download_slots": "2",
                "http_timeout": "7.5",
                "ovl_exclude_pol": "[0, 3]"
            }),
            ..Case::default()
        },
    ) else {
        return;
    };
    assert_eq!(o.code, Some(0), "{}", o.stdout);
    let tile = app_on(&o.log, "O4_Tile_Utils");
    assert_eq!(tile["max_download_slots"], 2);
    assert!(tile.get("http_timeout").is_none());
    let img = app_on(&o.log, "O4_Imagery_Utils");
    assert_eq!(img["http_timeout"], 7.5);
    assert!(img.get("max_download_slots").is_none());
    assert_eq!(
        app_on(&o.log, "O4_Overlay_Utils")["ovl_exclude_pol"],
        serde_json::json!([0, 3])
    );
    assert!(tile.get("ovl_exclude_pol").is_none());
    assert!(img.get("ovl_exclude_pol").is_none());
    let attrs = tile_attrs(&o.log);
    assert!(attrs.get("max_download_slots").is_none());
    assert!(attrs.get("http_timeout").is_none());
}

#[test]
fn overrides_apply_for_overlay_tasks_too() {
    let Some(o) = run_case(
        "overlay",
        "+50-002",
        &[],
        Case {
            app_overrides: serde_json::json!({"ovl_exclude_pol": "[5]"}),
            ..Case::default()
        },
    ) else {
        return;
    };
    assert_eq!(o.code, Some(0), "{}", o.stdout);
    assert_eq!(
        app_on(&o.log, "O4_Overlay_Utils")["ovl_exclude_pol"],
        serde_json::json!([5])
    );
}

#[test]
fn an_unknown_override_key_is_a_configure_failure() {
    let Some(o) = run_case(
        "ortho",
        "+50-002",
        &[],
        Case {
            app_overrides: serde_json::json!({"no_such_app_var": "1"}),
            ..Case::default()
        },
    ) else {
        return;
    };
    assert_configure_failure(&o, "no_such_app_var");
}

#[test]
fn a_tile_level_key_in_overrides_is_not_an_app_variable() {
    let Some(o) = run_case(
        "ortho",
        "+50-002",
        &[],
        Case {
            app_overrides: serde_json::json!({"cover_zl": "14"}),
            ..Case::default()
        },
    ) else {
        return;
    };
    assert_configure_failure(&o, "cover_zl");
    let reason = last_json(&o.stdout)["reason"].as_str().unwrap().to_string();
    assert!(reason.contains("tile-level"), "{reason}");
    assert!(reason.contains("raw"), "{reason}");
}

#[test]
fn skip_converts_in_overrides_is_refused_as_region_intent() {
    let Some(o) = run_case(
        "ortho",
        "+50-002",
        &[],
        Case {
            app_overrides: serde_json::json!({"skip_converts": "False"}),
            ..Case::default()
        },
    ) else {
        return;
    };
    assert_configure_failure(&o, "skip_converts");
    let reason = last_json(&o.stdout)["reason"].as_str().unwrap().to_string();
    assert!(reason.contains("spec"), "{reason}");
}

#[test]
fn a_missing_or_non_bool_skip_converts_fails_in_configure() {
    for wire in [serde_json::Value::Null, serde_json::json!("true")] {
        let Some(o) = run_case(
            "ortho",
            "+50-002",
            &[],
            Case {
                skip_converts_wire: Some(wire.clone()),
                ..Case::default()
            },
        ) else {
            return;
        };
        assert_configure_failure(&o, "skip_converts");
    }
}

#[test]
fn the_overlay_source_is_refused_in_overrides() {
    for key in ["custom_overlay_src", "custom_overlay_src_alternate"] {
        let Some(o) = run_case(
            "ortho",
            "+50-002",
            &[],
            Case {
                app_overrides: serde_json::json!({ key: "/elsewhere" }),
                ..Case::default()
            },
        ) else {
            return;
        };
        assert_configure_failure(&o, key);
    }
}

#[test]
fn an_app_level_key_in_raw_is_refused_for_every_task_type() {
    for t in ["ortho", "overlay"] {
        let Some(o) = run_raw(
            t,
            "+50-002",
            &[],
            serde_json::json!({"max_download_slots": "2"}),
        ) else {
            return;
        };
        assert_configure_failure(&o, "max_download_slots");
        let reason = last_json(&o.stdout)["reason"].as_str().unwrap().to_string();
        assert!(reason.contains("overrides"), "{t}: {reason}");
    }
}

#[test]
fn patch_presence_is_logged_when_the_tile_has_patches() {
    let Some(o) = run_case(
        "ortho",
        "+50-002",
        &[],
        Case {
            patches_dir: Some("+50-010/+50-002"),
            ..Case::default()
        },
    ) else {
        return;
    };
    assert_eq!(o.code, Some(0), "{}", o.stdout);
    assert!(
        o.stderr.contains("patches: present for +50-002"),
        "{}",
        o.stderr
    );
    assert!(!o.stderr.contains("patches: none"), "{}", o.stderr);
}

#[test]
fn patch_absence_is_logged_when_the_tile_has_none() {
    let Some(o) = run("ortho", "+50-002", &[]) else {
        return;
    };
    assert_eq!(o.code, Some(0), "{}", o.stdout);
    assert!(
        o.stderr.contains("patches: none for +50-002"),
        "{}",
        o.stderr
    );
}
