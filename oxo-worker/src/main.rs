use clap::Parser;
use oxo_worker::api::ControlPlane;
use oxo_worker::config::Config;
use oxo_worker::run::{run, Deps};

#[tokio::main]
async fn main() {
    // Without a subscriber every tracing event is dropped: the first
    // end-to-end run's worker log was empty for exactly this reason.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let config = Config::parse();
    let client = ControlPlane::new(&config.control_plane_url);
    let reason = run(&config, &client, Deps::from_config(&config)).await;
    tracing::info!(?reason, "exiting");
    std::process::exit(reason.exit_code());
}
