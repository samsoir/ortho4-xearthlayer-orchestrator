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

    /// Resolve a lease against one task, distinguishing the three ways it can
    /// be invalid. A single query so the checks cannot drift apart between
    /// the three reporting calls, and so the answer cannot change between
    /// two of them.
    ///
    /// `FOR UPDATE` is load-bearing, and here it genuinely locks: the row
    /// exists, so the lock holds for the caller's transaction and the reaper
    /// cannot reclaim the task between this check and the write that follows
    /// it. Without it the write's own `lease_token` predicate would match no
    /// rows and the call would report success having changed nothing -- the
    /// in-memory adapter holds its mutex across the whole operation, so it
    /// has no such window. Note the contrast with `create_job`, where the
    /// row does NOT yet exist and `FOR UPDATE` would lock nothing at all.
    async fn resolve<'e, E>(executor: E, lease: Lease) -> Result<(), TaskStoreError>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row: Option<(String, Option<Uuid>)> =
            sqlx::query_as("SELECT state, lease_token FROM tasks WHERE id = $1 FOR UPDATE")
                .bind(lease.task_id.as_uuid())
                .fetch_optional(executor)
                .await
                .map_err(adapter)?;

        let Some((state, token)) = row else {
            return Err(TaskStoreError::UnknownTask {
                task_id: lease.task_id,
            });
        };
        if state != "claimed" {
            return Err(TaskStoreError::NotClaimed {
                task_id: lease.task_id,
            });
        }
        if token != Some(lease.token.as_uuid()) {
            return Err(TaskStoreError::LeaseLost {
                task_id: lease.task_id,
            });
        }
        Ok(())
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
    async fn heartbeat(&self, lease: Lease) -> Result<(), TaskStoreError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(adapter)?;
        Self::resolve(&mut *tx, lease).await?;
        sqlx::query("UPDATE tasks SET last_heartbeat_at = $1 WHERE id = $2 AND lease_token = $3")
            .bind(now)
            .bind(lease.task_id.as_uuid())
            .bind(lease.token.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(adapter)?;
        tx.commit().await.map_err(adapter)
    }

    async fn complete(&self, lease: Lease) -> Result<(), TaskStoreError> {
        let mut tx = self.pool.begin().await.map_err(adapter)?;
        Self::resolve(&mut *tx, lease).await?;
        sqlx::query(
            "UPDATE tasks SET state = 'succeeded', lease_token = NULL, claimed_by = NULL, \
                 claimed_at = NULL, last_heartbeat_at = NULL \
             WHERE id = $1 AND lease_token = $2",
        )
        .bind(lease.task_id.as_uuid())
        .bind(lease.token.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(adapter)?;
        tx.commit().await.map_err(adapter)
    }

    async fn fail(&self, request: FailRequest) -> Result<FailOutcome, TaskStoreError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(adapter)?;
        Self::resolve(&mut *tx, request.lease).await?;

        let (attempts, max_attempts, backoff_secs): (i64, i64, i64) = sqlx::query_as(
            "SELECT t.attempts, j.max_attempts, j.backoff_secs \
             FROM tasks t JOIN jobs j ON j.id = t.job_id WHERE t.id = $1",
        )
        .bind(request.lease.task_id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(adapter)?;

        let outcome = if attempts >= max_attempts {
            sqlx::query(
                "UPDATE tasks SET state = 'abandoned', lease_token = NULL, claimed_by = NULL, \
                     claimed_at = NULL, last_heartbeat_at = NULL, last_failure = $2 \
                 WHERE id = $1",
            )
            .bind(request.lease.task_id.as_uuid())
            .bind(&request.reason)
            .execute(&mut *tx)
            .await
            .map_err(adapter)?;
            FailOutcome::Abandoned
        } else {
            let claimable_at = now + chrono::Duration::seconds(backoff_secs);
            sqlx::query(
                "UPDATE tasks SET state = 'pending', claimable_at = $2, lease_token = NULL, \
                     claimed_by = NULL, claimed_at = NULL, last_heartbeat_at = NULL, \
                     last_failure = $3 \
                 WHERE id = $1",
            )
            .bind(request.lease.task_id.as_uuid())
            .bind(claimable_at)
            .bind(&request.reason)
            .execute(&mut *tx)
            .await
            .map_err(adapter)?;
            FailOutcome::Requeued {
                claimable_at,
                attempts_remaining: u32::try_from(max_attempts - attempts).unwrap_or(0),
            }
        };

        tx.commit().await.map_err(adapter)?;
        Ok(outcome)
    }

    async fn reap_expired(&self, request: ReapRequest) -> Result<ReapOutcome, TaskStoreError> {
        let now = self.clock.now();
        let heartbeat_cutoff = now
            - chrono::Duration::from_std(request.heartbeat_timeout)
                .unwrap_or(chrono::Duration::zero());
        let duration_cutoff = now
            - chrono::Duration::from_std(request.max_task_duration)
                .unwrap_or(chrono::Duration::zero());

        // One statement so a concurrent reaper cannot double-count: each
        // expired row is updated by exactly one of them. The CASE spends the
        // start that was already consumed at claim, abandoning when the
        // budget is gone and requeueing with backoff otherwise.
        let rows: Vec<(String,)> = sqlx::query_as(
            "UPDATE tasks AS t SET \
                 state = CASE WHEN t.attempts >= j.max_attempts THEN 'abandoned' ELSE 'pending' END, \
                 claimable_at = CASE WHEN t.attempts >= j.max_attempts THEN t.claimable_at \
                                     ELSE $1 + (j.backoff_secs * interval '1 second') END, \
                 lease_token = NULL, claimed_by = NULL, claimed_at = NULL, \
                 last_heartbeat_at = NULL \
             FROM jobs AS j \
             WHERE j.id = t.job_id AND t.state = 'claimed' \
               AND (t.last_heartbeat_at < $2 OR t.claimed_at < $3) \
             RETURNING t.state",
        )
        .bind(now)
        .bind(heartbeat_cutoff)
        .bind(duration_cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(adapter)?;

        let mut outcome = ReapOutcome::default();
        for (state,) in rows {
            if state == "abandoned" {
                outcome.abandoned += 1;
            } else {
                outcome.requeued += 1;
            }
        }
        Ok(outcome)
    }
    async fn job_status(&self, _job_id: JobId) -> Result<JobStatus, TaskStoreError> {
        unimplemented!("Task 12")
    }
    async fn throughput(&self, _job_id: JobId) -> Result<Throughput, TaskStoreError> {
        unimplemented!("Task 12")
    }
}
