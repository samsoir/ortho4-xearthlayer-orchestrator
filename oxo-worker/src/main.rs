use clap::Parser;
use oxo_worker::api::ControlPlane;
use oxo_worker::config::Config;
use oxo_worker::run::{run, Deps};

#[tokio::main]
async fn main() {
    let config = Config::parse();
    let client = ControlPlane::new(&config.control_plane_url);
    let reason = run(&config, &client, Deps::from_config(&config)).await;
    tracing::info!(?reason, "exiting");
    std::process::exit(reason.exit_code());
}
