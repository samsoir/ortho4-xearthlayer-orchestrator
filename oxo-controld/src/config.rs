use clap::Parser;
use oxo_tasks::{InvalidQuantity, ReapRequest, TimeoutSeconds};

/// The control plane daemon's configuration. The two reap bounds are
/// stated guesses until spike 0 produces real tile timings; they are
/// flags precisely so the guess is cheap to correct.
#[derive(Debug, Parser)]
#[command(name = "oxo-controld", version, about)]
pub struct Config {
    /// Address to serve on. Loopback by default, so exposing the API
    /// beyond the host is an explicit act.
    #[arg(long, env = "OXO_BIND", default_value = "127.0.0.1:8080")]
    pub bind: String,

    /// PostgreSQL connection string for the task store.
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    /// Reclaim a claimed task if no heartbeat arrives within this.
    #[arg(long, env = "OXO_HEARTBEAT_TIMEOUT_SECS", default_value_t = 120)]
    pub heartbeat_timeout_secs: u64,

    /// Reclaim a claimed task held this long regardless of heartbeats.
    #[arg(long, env = "OXO_MAX_TASK_DURATION_SECS", default_value_t = 21_600)]
    pub max_task_duration_secs: u64,

    /// How often the reaper runs.
    #[arg(long, env = "OXO_REAP_INTERVAL_SECS", default_value_t = 30)]
    pub reap_interval_secs: u64,
}

impl Config {
    /// Validated reap bounds. Refusing zero here means a reclaim storm is
    /// a startup error, not a production discovery.
    pub fn reap_request(&self) -> Result<ReapRequest, InvalidQuantity> {
        Ok(ReapRequest {
            heartbeat_timeout: TimeoutSeconds::new(self.heartbeat_timeout_secs)?,
            max_task_duration: TimeoutSeconds::new(self.max_task_duration_secs)?,
        })
    }

    /// Validated reaper period. `tokio::time::interval` panics on zero,
    /// so zero must be a startup error rather than a dead reaper.
    pub fn reap_interval(&self) -> Result<std::time::Duration, InvalidQuantity> {
        Ok(std::time::Duration::from_secs(
            TimeoutSeconds::new(self.reap_interval_secs)?.get(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn defaults_are_the_design_documents() {
        let config = Config::try_parse_from(["oxo-controld", "--database-url", "postgres://x"])
            .expect("parse");
        assert_eq!(config.bind, "127.0.0.1:8080");
        assert_eq!(config.heartbeat_timeout_secs, 120);
        assert_eq!(config.max_task_duration_secs, 21_600);
        assert_eq!(config.reap_interval_secs, 30);
    }

    #[test]
    fn the_database_url_is_required() {
        assert!(Config::try_parse_from(["oxo-controld"]).is_err());
    }

    #[test]
    fn a_zero_reap_bound_is_refused_before_the_server_binds() {
        let config = Config::try_parse_from([
            "oxo-controld",
            "--database-url",
            "postgres://x",
            "--heartbeat-timeout-secs",
            "0",
        ])
        .expect("clap accepts the number; the quantity refuses it");
        assert!(config.reap_request().is_err());
    }

    #[test]
    fn valid_bounds_become_a_reap_request() {
        let config = Config::try_parse_from(["oxo-controld", "--database-url", "postgres://x"])
            .expect("parse");
        let request = config.reap_request().expect("valid defaults");
        assert_eq!(request.heartbeat_timeout.get(), 120);
        assert_eq!(request.max_task_duration.get(), 21_600);
    }

    #[test]
    fn a_zero_reap_interval_is_refused_before_the_server_binds() {
        let config = Config::try_parse_from([
            "oxo-controld",
            "--database-url",
            "postgres://x",
            "--reap-interval-secs",
            "0",
        ])
        .expect("clap accepts the number; the quantity refuses it");
        assert!(config.reap_interval().is_err());
    }

    #[test]
    fn the_default_reap_interval_is_thirty_seconds() {
        let config = Config::try_parse_from(["oxo-controld", "--database-url", "postgres://x"])
            .expect("parse");
        assert_eq!(
            config.reap_interval().expect("valid default"),
            std::time::Duration::from_secs(30)
        );
    }
}
