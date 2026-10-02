//! The conformance suite, run against a real PostgreSQL.
//!
//! Needs a database: run `make verify-db`, which starts a disposable one
//! via podman. These tests are excluded from `make verify` by name rather
//! than skipped at runtime, so a green `make verify` never implies the
//! adapter was exercised.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use oxo_tasks::clock::TestClock;
use oxo_tasks::conformance::{Fixture, Subject};
use oxo_tasks_postgres::PostgresTaskStore;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("DATABASE_URL").expect(
        "DATABASE_URL is unset. These tests need a database — run `make verify-db`, \
         which starts a disposable PostgreSQL via podman.",
    )
}

async fn pool() -> PgPool {
    PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url())
        .await
        .expect("connect to the test database")
}

#[tokio::test]
async fn the_schema_applies_and_is_idempotent() {
    let pool = pool().await;
    oxo_tasks_postgres::run_migrations(&pool)
        .await
        .expect("migrations apply");
    oxo_tasks_postgres::run_migrations(&pool)
        .await
        .expect("migrations are idempotent");

    let tables: Vec<String> =
        sqlx::query_scalar("SELECT tablename FROM pg_tables WHERE schemaname = current_schema()")
            .fetch_all(&pool)
            .await
            .expect("list tables");
    assert!(tables.iter().any(|t| t == "jobs"), "{tables:?}");
    assert!(tables.iter().any(|t| t == "tasks"), "{tables:?}");
}

/// Gives every case its own schema.
///
/// Cases run concurrently in one process, so truncating shared tables
/// between them would race and produce baffling failures. A schema per case
/// is real isolation, and `search_path` is set on every pooled connection
/// so the adapter needs no knowledge of it.
struct Postgres;

#[async_trait]
impl Fixture for Postgres {
    async fn fresh(&self) -> Subject {
        let schema = format!("conf_{}", Uuid::new_v4().simple());

        // One connection, not the shared pool's eight: this only issues a
        // CREATE SCHEMA, and eighteen cases running concurrently would
        // otherwise open well over a hundred connections between them and
        // exhaust the server's default limit. The pool is dropped at the end
        // of this function, so the connection does not outlive the setup.
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url())
            .await
            .expect("connect to create the schema");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create schema");

        let scoped = PgPoolOptions::new()
            .max_connections(8)
            .after_connect({
                let schema = schema.clone();
                move |conn, _meta| {
                    let schema = schema.clone();
                    Box::pin(async move {
                        sqlx::query(&format!("SET search_path TO {schema}"))
                            .execute(conn)
                            .await?;
                        Ok(())
                    })
                }
            })
            .connect(&database_url())
            .await
            .expect("connect with a scoped search_path");

        oxo_tasks_postgres::run_migrations(&scoped)
            .await
            .expect("migrations apply");

        let clock = Arc::new(TestClock::new(
            chrono::Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        ));
        Subject {
            store: Box::new(PostgresTaskStore::new(scoped, clock.clone())),
            clock,
        }
    }
}

oxo_tasks::conformance_suite!(Postgres);
