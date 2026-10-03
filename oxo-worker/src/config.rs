use std::collections::BTreeMap;

use clap::{Parser, ValueEnum};

/// What the pod does after a task's cleanup: take the next task in the
/// same pod, or exit and leave the platform to replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Recycle,
    Stop,
}

/// The worker supervisor's configuration. Injected at pod start; the pod
/// carries none of its own.
#[derive(Debug, Parser)]
#[command(name = "oxo-worker", version, about)]
pub struct Config {
    /// Base URL of the OXO control plane.
    #[arg(long, env = "OXO_CONTROL_URL")]
    pub control_plane_url: String,

    /// Identity reported on claims. Defaults to the hostname.
    #[arg(long, env = "OXO_WORKER_NAME")]
    pub worker_name: Option<String>,

    /// Recycle or stop after cleanup.
    #[arg(long, env = "OXO_MODE", value_enum, default_value = "recycle")]
    pub mode: Mode,

    /// Pause between claim attempts when there is no work or no room.
    #[arg(long, default_value_t = 15)]
    pub poll_interval_secs: u64,

    /// Heartbeat period while a task runs.
    #[arg(long, default_value_t = 30)]
    pub heartbeat_interval_secs: u64,

    /// Claim only if the scratch volume has at least this much free.
    // 8 GiB: the measured ZL16 floor — peak observed scratch was
    // 3.33 GiB on a heavy tile (docs/specs/2026-10-02-ortho4xp-pod-
    // contract.md, section h). Producing above ZL16 needs this raised
    // with the zoom (ZL17 projects ~13 GiB).
    #[arg(
        long,
        env = "OXO_MIN_FREE_SCRATCH_BYTES",
        default_value_t = 8_589_934_592
    )]
    pub min_free_scratch_bytes: u64,

    /// Ortho4XP installation root.
    #[arg(long, default_value = "/opt/ortho4xp")]
    pub install_root: String,

    /// Ephemeral scratch volume.
    #[arg(long, default_value = "/var/oxo/scratch")]
    pub scratch_dir: String,

    /// Read-only content directory.
    #[arg(long, default_value = "/var/oxo/content")]
    pub content_dir: String,

    /// Pod-level `custom_overlay_src`, handed to every runner invocation.
    #[arg(
        long,
        env = "OXO_OVERLAY_SRC",
        default_value = "/var/oxo/content/xplane"
    )]
    pub overlay_src: String,

    /// Pod-level overrides of Ortho4XP's app-level variables: a JSON object
    /// of variable names to string values, applied by the runner to each
    /// variable's owning module. Shape-checked here; names and values are
    /// checked by the runner against Ortho4XP's own variable table.
    #[arg(
        long,
        env = "OXO_O4_APP_OVERRIDES",
        default_value = "{}",
        value_parser = parse_app_overrides
    )]
    pub o4_app_overrides: BTreeMap<String, String>,

    /// Directory of site-specific Ortho4XP config files. Each regular
    /// top-level `*.txt` in it is copied over the same name in the install
    /// root at startup (e.g. `overpass_servers.txt`, `community_server.txt`).
    /// Empty or unset means no overlay.
    #[arg(long, env = "OXO_O4_CONFIG_OVERLAY")]
    pub o4_config_overlay: Option<String>,

    /// The Ortho4XP runner script.
    #[arg(long, default_value = "/opt/oxo/oxo_o4_runner.py")]
    pub runner: String,
}

/// A JSON object whose values are all strings; anything else refuses
/// startup rather than failing every task later.
fn parse_app_overrides(raw: &str) -> Result<BTreeMap<String, String>, String> {
    // A templated-but-unset env var arrives empty: that means no overrides.
    if raw.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("not valid JSON: {e}"))?;
    let serde_json::Value::Object(map) = value else {
        return Err("must be a JSON object of variable names to string values".into());
    };
    map.into_iter()
        .map(|(k, v)| match v {
            serde_json::Value::String(s) => Ok((k, s)),
            other => Err(format!("value for {k:?} must be a string, got {other}")),
        })
        .collect()
}

impl Config {
    /// The overlay directory, if one was asked for; empty counts as unset
    /// (a templated-but-unset env var arrives empty).
    pub fn config_overlay(&self) -> Option<&str> {
        self.o4_config_overlay.as_deref().filter(|p| !p.is_empty())
    }

    /// The configured name, else the kernel hostname, else a fixed
    /// fallback — a claim must always carry some identity.
    pub fn worker_name(&self) -> String {
        if let Some(name) = self.worker_name.as_deref().filter(|n| !n.is_empty()) {
            return name.to_string();
        }
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|h| h.trim().to_string())
            .ok()
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "oxo-worker".to_string())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    const BIN: &str = "oxo-worker";

    fn parse(extra: &[&str]) -> Result<Config, clap::Error> {
        let mut args = vec![BIN];
        args.extend_from_slice(extra);
        Config::try_parse_from(args)
    }

    #[test]
    fn defaults_are_the_pod_layout() {
        let c = parse(&["--control-plane-url", "http://cp:8080"]).expect("parse");
        assert_eq!(c.control_plane_url, "http://cp:8080");
        assert_eq!(c.mode, Mode::Recycle);
        assert_eq!(c.poll_interval_secs, 15);
        assert_eq!(c.heartbeat_interval_secs, 30);
        assert_eq!(c.min_free_scratch_bytes, 8_589_934_592);
        assert_eq!(c.install_root, "/opt/ortho4xp");
        assert_eq!(c.scratch_dir, "/var/oxo/scratch");
        assert_eq!(c.content_dir, "/var/oxo/content");
        assert_eq!(c.overlay_src, "/var/oxo/content/xplane");
        assert!(c.o4_app_overrides.is_empty());
        assert_eq!(c.o4_config_overlay, None);
        assert_eq!(c.runner, "/opt/oxo/oxo_o4_runner.py");
    }

    #[test]
    fn the_control_plane_url_is_required() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn mode_parses_both_values_and_refuses_others() {
        let stop = parse(&["--control-plane-url", "u", "--mode", "stop"]).expect("parse");
        assert_eq!(stop.mode, Mode::Stop);
        let recycle = parse(&["--control-plane-url", "u", "--mode", "recycle"]).expect("parse");
        assert_eq!(recycle.mode, Mode::Recycle);
        assert!(parse(&["--control-plane-url", "u", "--mode", "pause"]).is_err());
    }

    #[test]
    fn an_explicit_worker_name_wins_over_the_hostname() {
        let c = parse(&["--control-plane-url", "u", "--worker-name", "pod-7"]).expect("parse");
        assert_eq!(c.worker_name(), "pod-7");
    }

    #[test]
    fn the_worker_name_is_never_empty_without_a_flag() {
        let c = parse(&["--control-plane-url", "u"]).expect("parse");
        assert!(!c.worker_name().is_empty());
    }

    #[test]
    fn app_overrides_accept_a_json_object_of_strings() {
        let c = parse(&[
            "--control-plane-url",
            "u",
            "--o4-app-overrides",
            r#"{"max_download_slots":"2","http_timeout":"7.5"}"#,
        ])
        .expect("parse");
        assert_eq!(c.o4_app_overrides["max_download_slots"], "2");
        assert_eq!(c.o4_app_overrides["http_timeout"], "7.5");
        assert_eq!(c.o4_app_overrides.len(), 2);
    }

    #[test]
    fn app_overrides_refuse_a_non_object() {
        for bad in ["[]", "\"x\"", "3", "not json"] {
            assert!(
                parse(&["--control-plane-url", "u", "--o4-app-overrides", bad]).is_err(),
                "{bad:?} must refuse startup"
            );
        }
    }

    #[test]
    fn app_overrides_refuse_non_string_values() {
        for bad in [
            r#"{"a":2}"#,
            r#"{"a":true}"#,
            r#"{"a":null}"#,
            r#"{"a":["1"]}"#,
        ] {
            assert!(
                parse(&["--control-plane-url", "u", "--o4-app-overrides", bad]).is_err(),
                "{bad:?} must refuse startup"
            );
        }
    }

    #[test]
    fn empty_app_overrides_mean_none() {
        for empty in ["", "  "] {
            let c =
                parse(&["--control-plane-url", "u", "--o4-app-overrides", empty]).expect("parse");
            assert!(c.o4_app_overrides.is_empty());
        }
    }

    #[test]
    fn config_overlay_is_an_optional_path() {
        let c =
            parse(&["--control-plane-url", "u", "--o4-config-overlay", "/etc/o4"]).expect("parse");
        assert_eq!(c.o4_config_overlay.as_deref(), Some("/etc/o4"));
    }
}
