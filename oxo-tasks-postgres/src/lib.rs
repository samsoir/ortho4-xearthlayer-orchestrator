//! PostgreSQL adapter for the OXO task store.
//!
//! Claiming uses `SELECT … FOR UPDATE SKIP LOCKED`, PostgreSQL's own idiom
//! for handing one row to exactly one of many concurrent consumers.
//!
//! Every timestamp arrives as a query parameter from the injected
//! [`Clock`](oxo_tasks::Clock). This adapter never calls SQL `now()`: that
//! would put the application and the database on separate clocks, so skew
//! between them would silently change when a task is reclaimed, and it would
//! make expiry testable only by sleeping.

#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use oxo_tasks::clock::Clock;
use oxo_tasks::error::TaskStoreError;
use oxo_tasks::ids::{JobId, LeaseToken, TaskId};
use oxo_tasks::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, JobCreated, JobStatus, Lease,
    ReapOutcome, ReapRequest, Throughput,
};
use oxo_tasks::store::TaskStore;
use oxo_tasks::task::TaskType;
use sqlx::PgPool;
use uuid::Uuid;

/// Apply the schema. Idempotent; safe to call on every start.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

/// A task store backed by PostgreSQL.
#[derive(Clone)]
pub struct PostgresTaskStore {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl PostgresTaskStore {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }
}

/// Adapter failures become `TaskStoreError::Adapter`, which is the only
/// variant a caller may consider retrying.
fn adapter(error: sqlx::Error) -> TaskStoreError {
    TaskStoreError::Adapter(error.to_string())
}

#[async_trait]
impl TaskStore for PostgresTaskStore {
    async fn create_job(&self, request: CreateJob) -> Result<JobCreated, TaskStoreError> {
        let mut seen = std::collections::BTreeSet::new();
        for spec in &request.tasks {
            if !seen.insert((spec.tile, spec.task_type)) {
                return Err(TaskStoreError::DuplicateTask {
                    tile: spec.tile,
                    task_type: spec.task_type,
                });
            }
        }

        if request.tasks.is_empty() {
            return Err(TaskStoreError::EmptyJob {
                region_code: request.region_code,
                revision: request.revision,
            });
        }

        let now = self.clock.now();
        // Both conversions below are infallible: the columns are bigint, and
        // every u32 fits in an i64. Saturating instead would be silent
        // corruption -- two different revisions mapping onto one stored value
        // would make two distinct jobs share an identity, and
        // (region_code, revision) is exactly what decides whether a request
        // resumes an existing job or starts a new one.
        let max_attempts = i64::from(request.max_attempts);
        let revision = i64::from(request.revision);
        // A backoff is a u64 of seconds, which genuinely can exceed i64. It is
        // refused rather than saturated, because a silently shortened backoff
        // would let a failing tile burn its whole attempt budget at once.
        let backoff_secs = i64::try_from(request.backoff.as_secs()).map_err(|_| {
            TaskStoreError::Adapter(format!(
                "backoff of {} seconds exceeds the representable range",
                request.backoff.as_secs()
            ))
        })?;

        let mut tx = self.pool.begin().await.map_err(adapter)?;

        // Insert first, then fall back to resuming. The obvious order --
        // look for the identity, then insert if absent -- does not work
        // here: `SELECT ... FOR UPDATE` locks rows that EXIST, so a
        // brand-new identity has no row to lock, two concurrent creators
        // both find nothing, and both insert. One then receives a bare
        // unique violation where the in-memory adapter, serialised by its
        // mutex, resumes. That is not hypothetical for OXO, where dispatch
        // is pull and several pods can call create_job for the same region
        // as they start.
        //
        // Inserting first makes the unique index itself the arbiter. The
        // loser's statement waits for the winner's transaction to finish;
        // if it committed, DO NOTHING applies and we fall through to the
        // resume path, where the winner's tasks are already visible because
        // they were committed in the same transaction. If it aborted, no
        // conflict remains and this insert simply succeeds. The fall-through
        // relies on READ COMMITTED, PostgreSQL's default, where each
        // statement sees the latest committed data.
        let job_uuid = Uuid::new_v4();
        let inserted: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO jobs (id, region_code, revision, max_attempts, backoff_secs, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (region_code, revision) DO NOTHING \
             RETURNING id",
        )
        .bind(job_uuid)
        .bind(&request.region_code)
        .bind(revision)
        .bind(max_attempts)
        .bind(backoff_secs)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(adapter)?;

        if inserted.is_none() {
            // DO NOTHING fired, so a committed row holds this identity --
            // an aborted one would have left no conflict and let the insert
            // through. Reading it back cannot come up empty.
            let (existing_uuid, existing_attempts, existing_backoff): (Uuid, i64, i64) =
                sqlx::query_as(
                    "SELECT id, max_attempts, backoff_secs FROM jobs \
                     WHERE region_code = $1 AND revision = $2",
                )
                .bind(&request.region_code)
                .bind(revision)
                .fetch_one(&mut *tx)
                .await
                .map_err(adapter)?;

            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT tile, task_type FROM tasks WHERE job_id = $1")
                    .bind(existing_uuid)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(adapter)?;

            let mut existing_set: Vec<(String, String)> = rows;
            existing_set.sort();
            // No dedup: the duplicate guard at the top of this function has
            // already proved the request carries no repeated pair, and the
            // table's UNIQUE constraint says the same of the stored side.
            let mut requested: Vec<(String, String)> = request
                .tasks
                .iter()
                .map(|spec| (spec.tile.to_string(), spec.task_type.as_str().to_string()))
                .collect();
            requested.sort();

            let same_policy = existing_attempts == max_attempts && existing_backoff == backoff_secs;
            if existing_set != requested || !same_policy {
                return Err(TaskStoreError::JobConflict {
                    region_code: request.region_code,
                    revision: request.revision,
                });
            }

            tx.commit().await.map_err(adapter)?;
            let total_tasks = u32::try_from(existing_set.len()).unwrap_or(u32::MAX);
            return Ok(JobCreated {
                job_id: JobId::from_uuid(existing_uuid),
                created: false,
                total_tasks,
            });
        }

        for spec in &request.tasks {
            sqlx::query(
                // No ON CONFLICT clause. The duplicate guard makes a
                // collision within one request impossible, and job_uuid is
                // freshly minted so it cannot collide across jobs. A
                // conflict here would mean an assumption has broken, and
                // should fail loudly rather than quietly create fewer tasks
                // than were asked for.
                "INSERT INTO tasks (id, job_id, tile, task_type, state, attempts, claimable_at) \
                 VALUES ($1, $2, $3, $4, 'pending', 0, $5)",
            )
            .bind(Uuid::new_v4())
            .bind(job_uuid)
            .bind(spec.tile.to_string())
            .bind(spec.task_type.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(adapter)?;
        }

        let total_tasks: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE job_id = $1")
            .bind(job_uuid)
            .fetch_one(&mut *tx)
            .await
            .map_err(adapter)?;

        tx.commit().await.map_err(adapter)?;

        Ok(JobCreated {
            job_id: JobId::from_uuid(job_uuid),
            created: true,
            total_tasks: u32::try_from(total_tasks).unwrap_or(u32::MAX),
        })
    }
    async fn claim(&self, request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError> {
        let now = self.clock.now();
        let token = LeaseToken::generate();
        let wanted: Option<Vec<String>> = request.task_types.as_ref().map(|types| {
            types
                .iter()
                .map(|task_type| task_type.as_str().to_string())
                .collect()
        });

        let row: Option<(Uuid, Uuid, String, String, i64)> = sqlx::query_as(
            "UPDATE tasks SET \
                 state = 'claimed', lease_token = $1, claimed_by = $2, \
                 claimed_at = $3, last_heartbeat_at = $3, attempts = attempts + 1 \
             WHERE id = ( \
                 SELECT id FROM tasks \
                 WHERE state = 'pending' AND claimable_at <= $3 \
                   AND ($4::text[] IS NULL OR task_type = ANY($4)) \
                 ORDER BY claimable_at, id \
                 FOR UPDATE SKIP LOCKED \
                 LIMIT 1 \
             ) \
             RETURNING id, job_id, tile, task_type, attempts",
        )
        .bind(token.as_uuid())
        .bind(&request.worker)
        .bind(now)
        .bind(wanted.as_deref())
        .fetch_optional(&self.pool)
        .await
        .map_err(adapter)?;

        let Some((task_uuid, job_uuid, tile, task_type, attempts)) = row else {
            return Ok(None);
        };

        Ok(Some(ClaimedTask {
            task_id: TaskId::from_uuid(task_uuid),
            job_id: JobId::from_uuid(job_uuid),
            lease: token,
            tile: tile.parse().map_err(|error| {
                TaskStoreError::Adapter(format!(
                    "stored tile {tile:?} is not a valid identifier: {error}"
                ))
            })?,
            task_type: TaskType::from_str_exact(&task_type).ok_or_else(|| {
                TaskStoreError::Adapter(format!("stored task type {task_type:?} is not recognised"))
            })?,
            attempt: u32::try_from(attempts).unwrap_or(u32::MAX),
        }))
    }
    async fn heartbeat(&self, _lease: Lease) -> Result<(), TaskStoreError> {
        unimplemented!("Task 11")
    }
    async fn complete(&self, _lease: Lease) -> Result<(), TaskStoreError> {
        unimplemented!("Task 11")
    }
    async fn fail(&self, _request: FailRequest) -> Result<FailOutcome, TaskStoreError> {
        unimplemented!("Task 11")
    }
    async fn reap_expired(&self, _request: ReapRequest) -> Result<ReapOutcome, TaskStoreError> {
        unimplemented!("Task 11")
    }
    async fn job_status(&self, _job_id: JobId) -> Result<JobStatus, TaskStoreError> {
        unimplemented!("Task 12")
    }
    async fn throughput(&self, _job_id: JobId) -> Result<Throughput, TaskStoreError> {
        unimplemented!("Task 12")
    }
}
