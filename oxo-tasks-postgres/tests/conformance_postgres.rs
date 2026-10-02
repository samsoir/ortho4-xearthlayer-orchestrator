//! The conformance suite, run against a real PostgreSQL.
//!
//! Needs a database: run `make verify-db`, which starts a disposable one
//! via podman. These tests are excluded from `make verify` by name rather
//! than skipped at runtime, so a green `make verify` never implies the
//! adapter was exercised.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

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
