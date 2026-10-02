//! Composition root: the only code that knows PostgreSQL exists.

mod config;

use std::sync::Arc;

use clap::Parser;
use oxo_control::{api, reaper};
use oxo_tasks_postgres::{run_migrations, PostgresTaskStore};
use sqlx::postgres::PgPoolOptions;

use crate::config::Config;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::parse();
    let reap_request = config.reap_request()?;
    let reap_interval = config.reap_interval()?;

    let pool = PgPoolOptions::new().connect(&config.database_url).await?;
    run_migrations(&pool).await?;

    let clock = Arc::new(oxo_tasks::SystemClock);
    let store: Arc<dyn oxo_tasks::TaskStore> = Arc::new(PostgresTaskStore::new(pool, clock));

    let reaper = tokio::spawn(reaper::run(Arc::clone(&store), reap_request, reap_interval));

    let app = api::router(store).layer(tower_http::trace::TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(bind = %config.bind, "oxo-controld serving");
    tokio::select! {
        served = axum::serve(listener, app) => {
            served?;
            Ok(())
        }
        // The reaper runs forever; its ending means it panicked, and a
        // server without lease expiry must not keep serving.
        ended = reaper => {
            tracing::error!(?ended, "the reaper task ended; lease expiry is dead, exiting");
            Err("reaper task ended unexpectedly".into())
        }
    }
}
