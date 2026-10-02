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
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::parse();
    let reap_request = config.reap_request()?;

    let pool = PgPoolOptions::new().connect(&config.database_url).await?;
    run_migrations(&pool).await?;

    let clock = Arc::new(oxo_tasks::SystemClock);
    let store: Arc<dyn oxo_tasks::TaskStore> = Arc::new(PostgresTaskStore::new(pool, clock));

    tokio::spawn(reaper::run(
        Arc::clone(&store),
        reap_request,
        std::time::Duration::from_secs(config.reap_interval_secs),
    ));

    let app = api::router(store).layer(tower_http::trace::TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(bind = %config.bind, "oxo-controld serving");
    axum::serve(listener, app).await?;
    Ok(())
}
