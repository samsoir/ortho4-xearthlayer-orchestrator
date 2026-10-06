# Read-Only Console (Phase 5a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The read side of OXO's operator interface. The task store starts recording what the operator needs (attempt history, worker last-seen, the stored spec, delivered bytes and a masked log tail). A second port and a set of open read endpoints serve those records. A new `oxo-console` process renders them as the read-only Dashboard, Job detail (map and list) and Workers screens.

**Architecture:** Bottom-up, in the order the data flows. The `TaskStore` port gains the writes (each existing call records its attempt and worker facts in the same transaction as its transition). A new `OperatorQueries` port in `oxo-tasks` answers the reads, implemented by both adapters and held to its own conformance suite. `oxo-control` derives worker state and ETA from those facts and serves them as JSON. `oxo-worker` reports a per-boot identity, delivered bytes and a masked log tail. `oxo-console` is a new axum binary that renders askama templates from `oxo-controld`'s API, polls server-rendered fragments, and draws the tile map with Mapbox GL JS when the operator supplies a public token.

**Tech Stack:** Rust (edition 2021), axum 0.8, sqlx 0.8 (PostgreSQL), askama (templates), reqwest 0.12 (console to `oxo-controld`), `cucumber` 0.20.2 (acceptance), plain ES modules with Node's built-in test runner (`node --test`) for the islands, Mapbox GL JS from Mapbox's CDN, Inter and JetBrains Mono (OFL) embedded as `woff2`.

**Spec:** `docs/specs/2026-10-06-read-only-console-design.md` (phase 5a), under the umbrella `docs/specs/2026-10-04-operator-interface-design.md`. The gap numbers cited below come from `docs/specs/2026-10-04-web-ui-service-gaps.md`. Screens are in Figma file `dJyiarI0mfqgfCVifUNlO4`, page "02 · Screens": `19:2065` (08, Dashboard read-only), `3:193` (02, Job detail map), `6:2005` (06, Job detail list), `8:2045` (07, Workers). They are the visual reference, but the spec governs what 5a renders. In particular, 5a renders **no action buttons**, and no Paused, Cancelled, Superseded or retry-lineage states.

## Global Constraints

Every task's requirements implicitly include this section.

- **TDD is mandatory.** Write the failing test first, capture the real RED output, then write the minimal code to reach green. Where a task asserts behaviour that already ships, the RED step is a **bite check**: run once with a wrong expectation, show the failure, then restore.
- **Run `make verify` before every commit.** Tasks touching `oxo-tasks-postgres` also run `make verify-db`. The one exception is Task 1: it changes the port, so it deliberately ends with the PostgreSQL adapter uncompilable and `verify`'s lint gate and `verify-db` red. Its commit message says so, and Task 2 restores both. Tasks 1 and 2 are adjacent for exactly this reason, as are Tasks 3 and 4 (the same pattern for the read port).
- **Ports stay serde-free and sqlx-free.** `oxo-tasks` gains no `sqlx` and no `serde` dependency. Wire types live in `oxo-control::api::wire`, as today.
- **The control plane never depends on PostgreSQL.** `oxo-control` depends on `TaskStore` and `OperatorQueries` only; `oxo-controld` is the only crate naming `oxo-tasks-postgres`.
- **`oxo-console` depends on no store crate.** It may depend on `oxo-control` for the wire types only (`oxo_control::api::wire`), never on `oxo-tasks`, `oxo-tasks-postgres` or `sqlx`. It talks to `oxo-controld` over HTTP and holds no job, task or worker state.
- **`oxo-worker` production code depends on no workspace crate** (unchanged rule). Its dev-dependencies may run the real router in-process.
- **No wall-clock in tests.** Store time comes from `TestClock`; worker intervals are injected `Duration`s. The real-run gate (Task 19) is the sanctioned exception.
- **Alert destinations never leave through a read.** No endpoint, fragment or page returns `jobs.alert_destinations`. Task 7 adds a guard test that seeds a webhook URL and searches every read response for it.
- **Additive wire changes only.** Existing request and response shapes keep working; new body fields are optional. `GET /api/v1/jobs?region_code=&revision=` keeps today's exact behaviour.
- **Commit messages end with** `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`, preceded by a blank line, or git parses no trailer. Verify with `git log -1 --format='%(trailers)'`.
- **After committing, confirm what you committed:** `git status --porcelain` is empty, and `git show HEAD:<path>` contains the change.
- **Do not state derived counts in prose; name the things.** The conformance parity tests (`all_cases_are_registered` and its new operator twin) remain the enforced counts.

### Vocabulary

A **job** is one submission of one spec revision; a **task** is one tile's ortho or overlay conversion; plan units are capitalised **Tasks**. An **attempt** is one claim of one task, from claim to its end (complete, fail, reap). A worker's **identity** is the per-boot string it reports in the claim's `worker` field. The **read port** is `OperatorQueries`; the **store** is `TaskStore`. **Facts** are what the read port returns; **derived** values (worker state, ETA) are computed in `oxo-control` from facts plus configuration.

### Shared definitions (normative for the Tasks named)

**Attempt outcomes** (Tasks 1–4, 6, 7): `succeeded`, `failed`, `reclaimed_heartbeat`, `reclaimed_max_duration`, `revoked`, `superseded`. 5a writes only the first four. Port enum `AttemptOutcome` with `as_str`/`from_str_exact` in the style of `TaskState`.

**Reaper reasons** (Tasks 1, 2), written to the task's `last_failure` and the attempt's `reason`:
- heartbeat lapse: `reclaimed: no heartbeat for {heartbeat_timeout} s`
- backstop: `reclaimed: held longer than {max_task_duration} s`

When both apply, the heartbeat reason wins (it is the likelier cause).

**Log tail cap** (Tasks 5, 10): `LOG_TAIL_MAX_BYTES = 65_536`, defined once in `oxo-control::api::wire` and duplicated, with a test asserting equality, in `oxo-worker` (which may not depend on `oxo-control`).

**Liveness thresholds** (Tasks 6–8, 15): `late_secs` (default 60), `idle_secs` (default 120), `gone_secs` (default 600), plus the reaper's `heartbeat_timeout_secs` (120). Valid when `0 < late_secs < heartbeat_timeout_secs` and `0 < idle_secs < gone_secs`.

**Derived worker states** (Task 6). Evaluate in this order, where `now` is the read port's `as_of`:

| State | Rule |
|---|---|
| `reaping` | holds a lease, and `now − last_heartbeat_at > heartbeat_timeout_secs` |
| `late` | holds a lease, and `now − last_heartbeat_at > late_secs` |
| `working` | holds a lease |
| `gone_quiet` | no lease, and `now − last_seen_at > gone_secs` |
| `stale` | no lease, and `now − last_seen_at > idle_secs` |
| `idle` | no lease |

For the workers-page groups and header counts: **busy** = working + late + reaping; **idle** = idle + stale; **gone quiet** = gone_quiet.

**ETA** (Task 6). For each task type:

> `remaining = pending + claimed` (current tasks), `median` = the median duration (`ended_at − claimed_at`) of `succeeded` attempts of that type, `busy` = workers currently holding a lease of that type, minimum 1.

The ETA is `max over types of ceil(remaining × median ÷ busy)` seconds. It is `null` when any type with `remaining > 0` has fewer than 5 succeeded attempts, and `0` when nothing remains.

---

### Task 1: The store records attempts and workers (port and in-memory adapter)

**Files:**
- Modify: `oxo-tasks/src/request.rs`, `oxo-tasks/src/task.rs` (`AttemptOutcome`), `oxo-tasks/src/store.rs` (doc comments), `oxo-tasks/src/memory.rs`, `oxo-tasks/src/lib.rs`, `oxo-tasks/src/conformance.rs`
- Modify callers so the workspace (except `oxo-tasks-postgres`) compiles: `oxo-control/src/api/tasks.rs`, `oxo-control/src/planner.rs`, test fixtures constructing `CreateJob`, `FailRequest` or calling `complete`

**Interfaces:**
- Produces, in `oxo_tasks`:
  - `CreateJob` gains `pub name: String`, `pub spec_toml: String`, `pub alert_destinations: Vec<String>`
  - `pub struct CompleteRequest { pub lease: Lease, pub delivered_bytes: Option<u64>, pub log_tail: Option<String> }`
  - `FailRequest` gains `pub log_tail: Option<String>`
  - `TaskStore::complete(&self, request: CompleteRequest) -> Result<(), TaskStoreError>` (was `lease: Lease`)
  - `pub enum AttemptOutcome { Succeeded, Failed, ReclaimedHeartbeat, ReclaimedMaxDuration, Revoked, Superseded }` with `as_str` / `from_str_exact` / `ALL`, exported from `lib.rs`
- Behaviour (in-memory adapter, held by new conformance cases):
  - `create_job`: stores `name`, `spec_toml` and `alert_destinations`. The conflict comparison now includes `alert_destinations` as an **ordered** list (the order the spec gave), alongside the existing task set, policy and payload. `name` and `spec_toml` are not compared: they are fully determined by the payload, policy and task set except for `metadata.name`, and a renamed resubmission resumes, as today.
  - `claim`: always upserts the worker (`first_seen_at` set once, `last_seen_at = now`, `last_task_types` = the request's filter), **including when it returns `Ok(None)`**. On a hit it opens an attempt (`attempt_no` = the task's new attempt count, `claimed_at = now`, outcome none) and sets the task's `updated_at = now`.
  - `heartbeat`: on success, sets `last_seen_at = now` for the worker named by the task's `claimed_by`.
  - `complete`: closes the open attempt (`ended_at = now`, `succeeded`, `delivered_bytes`, `log_tail`) and sets `updated_at`.
  - `fail`: closes it (`failed`, `reason`, `log_tail`) and sets `updated_at`.
  - `reap_expired`: closes it (`reclaimed_heartbeat` or `reclaimed_max_duration`, `reason` = the reaper reason), writes the same reason to the task's `last_failure`, and sets `updated_at`.
  - `create_job` sets each new task's `updated_at` to the job's creation time.
  - The in-memory adapter keeps attempts and workers in its locked state, beside jobs and tasks. They are not readable through `TaskStore`; Task 3 adds the read port.

- [ ] **Step 1: Write the failing conformance cases.** They can only observe through `TaskStore` for now, so this Task's cases cover what is observable today:
  - `a_resubmission_with_different_alert_destinations_conflicts`
  - `a_resubmission_with_reordered_alert_destinations_conflicts`
  - `a_renamed_resubmission_resumes` (a different `name` with everything else equal gives `created: false`)
  - `complete_accepts_delivered_bytes_and_a_log_tail` (then a second `complete` with the same lease is still `NotClaimed`)
  - `fail_accepts_a_log_tail`

  The attempt-row and worker-row behaviour is asserted through the read port in Task 3, which is why this Task's in-memory state for it is written but read by nothing yet. Register every case in `conformance_suite!`, and confirm `all_cases_are_registered` fails before registration and passes after.
- [ ] **Step 2: Run** `cargo test -p oxo-tasks --all-features`. Expected: the new cases fail to compile (the new fields do not exist yet).
- [ ] **Step 3: Implement** the port changes and the in-memory behaviour above, and update every caller outside `oxo-tasks-postgres`:
  - `oxo-control` passes `CompleteRequest { delivered_bytes: None, log_tail: None, .. }` and `log_tail: None` until Task 5.
  - The planner passes `name: spec.metadata.name.clone()`, `spec_toml: String::new()` and `alert_destinations: spec.failure_policy.alert_destinations.clone()` until Task 5.
- [ ] **Step 4: Run** `cargo test --workspace --exclude oxo-tasks-postgres --all-features`. Expected: green. `make verify` fails in clippy on `oxo-tasks-postgres`, which is expected and stated in the commit.
- [ ] **Step 5: Commit** `feat(tasks): the store records attempts and workers (port + in-memory)`. The body states that `oxo-tasks-postgres` does not compile until the next commit.

### Task 2: The PostgreSQL adapter records attempts and workers

**Files:**
- Create: `oxo-tasks-postgres/migrations/0003_operator_reads.sql`
- Modify: `oxo-tasks-postgres/src/lib.rs`

**Interfaces:**
- Consumes: Task 1's port.
- Produces: migration `0003_operator_reads.sql`, exactly as below. The adapter implements Task 1's behaviour, each write in the same transaction as the transition it records.

```sql
-- 0003_operator_reads.sql
-- What the operator interface reads: per-claim attempt history, worker
-- last-seen, and the stored specification. Alert destinations are stored
-- apart from the spec so that no read can return them.

CREATE TABLE workers (
    identity        text        PRIMARY KEY,
    first_seen_at   timestamptz NOT NULL,
    last_seen_at    timestamptz NOT NULL,
    -- null = any type; else the filter's labels joined with ','
    last_task_types text
);

CREATE TABLE attempts (
    id              uuid        PRIMARY KEY,
    task_id         uuid        NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    job_id          uuid        NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    worker          text        NOT NULL,
    attempt_no      bigint      NOT NULL CHECK (attempt_no >= 1),
    claimed_at      timestamptz NOT NULL,
    ended_at        timestamptz,
    outcome         text CHECK (outcome IN ('succeeded', 'failed', 'reclaimed_heartbeat',
                                            'reclaimed_max_duration', 'revoked', 'superseded')),
    reason          text,
    delivered_bytes bigint CHECK (delivered_bytes >= 0),
    log_tail        text,
    CHECK ((ended_at IS NULL) = (outcome IS NULL)),
    UNIQUE (task_id, attempt_no)
);

-- At most one open attempt per task.
CREATE UNIQUE INDEX attempts_one_open ON attempts (task_id) WHERE ended_at IS NULL;
-- Worker history, newest first.
CREATE INDEX attempts_by_worker ON attempts (worker, claimed_at DESC);

ALTER TABLE jobs ADD COLUMN name text;
ALTER TABLE jobs ADD COLUMN spec_toml text;
ALTER TABLE jobs ADD COLUMN alert_destinations text[] NOT NULL DEFAULT '{}';

ALTER TABLE tasks ADD COLUMN updated_at timestamptz;
UPDATE tasks t SET updated_at = j.created_at FROM jobs j WHERE j.id = t.job_id;
ALTER TABLE tasks ALTER COLUMN updated_at SET NOT NULL;
-- The task list's sort key.
CREATE INDEX tasks_by_updated ON tasks (job_id, updated_at DESC, id);
```

- [ ] **Step 1: Bite check.** `make verify-db` is red from Task 1 (the adapter does not compile). Capture that as this Task's RED.
- [ ] **Step 2: Write the migration** and implement the adapter:
  - `create_job` inserts the three new job columns; the conflict path compares `alert_destinations` with ordered array equality.
  - `claim` upserts the worker with `INSERT … ON CONFLICT (identity) DO UPDATE SET last_seen_at, last_task_types`, **before** looking for a task, so a miss still records the worker. On a hit it inserts the attempt in the same transaction as the claim's `UPDATE`.
  - `heartbeat` updates `workers.last_seen_at` for `claimed_by`.
  - `complete` and `fail` close the open attempt.
  - The reaper's existing `UPDATE … RETURNING` gains `t.id, t.claimed_by` and the lapse kind, so it can close each attempt and write `last_failure` in the same statement or transaction. Keep the single-reaper deadlock note accurate.
- [ ] **Step 3: Run** `make verify-db`. Expected: green, including Task 1's new cases and the existing migration-idempotency test, now covering `0003`.
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(tasks-postgres): attempts, workers and the stored spec (migration 0003)`.

### Task 3: The read port and its in-memory implementation

**Files:**
- Create: `oxo-tasks/src/queries.rs` (port and row types), `oxo-tasks/src/operator_conformance.rs` (behind the `conformance` feature), `oxo-tasks/tests/operator_conformance_memory.rs`
- Modify: `oxo-tasks/src/lib.rs`, `oxo-tasks/src/memory.rs`, `oxo-tasks/src/conformance.rs` (`Subject`)

**Interfaces:**
- Produces, in `oxo_tasks::queries` (re-exported at the crate root), all plain Rust with no serde:

```rust
#[async_trait]
pub trait OperatorQueries: Send + Sync {
    /// Ok when the backing store answers and its schema is current.
    async fn ready(&self) -> Result<(), TaskStoreError>;
    async fn list_jobs(&self, query: JobListQuery) -> Result<Page<JobRow>, TaskStoreError>;
    async fn job_summary(&self, job_id: JobId) -> Result<JobSummary, TaskStoreError>;
    /// The stored spec TOML; Ok(None) for a job created before it was stored.
    async fn job_spec(&self, job_id: JobId) -> Result<Option<String>, TaskStoreError>;
    async fn tile_states(&self, job_id: JobId) -> Result<Vec<TileStates>, TaskStoreError>;
    async fn list_tasks(&self, job_id: JobId, query: TaskQuery) -> Result<Page<TaskRow>, TaskStoreError>;
    async fn task_attempts(&self, task_id: TaskId) -> Result<Vec<AttemptRow>, TaskStoreError>;
    async fn list_workers(&self) -> Result<WorkersSnapshot, TaskStoreError>;
    async fn worker_attempts(&self, query: WorkerAttemptsQuery) -> Result<Page<AttemptRow>, TaskStoreError>;
}

pub struct Page<T> { pub items: Vec<T>, pub next_cursor: Option<String> }

pub struct JobListQuery { pub cursor: Option<String>, pub limit: u32 }        // limit 1..=200
pub struct JobRow {
    pub job_id: JobId, pub region_code: String, pub revision: u32,
    pub name: Option<String>, pub status: JobStatus, pub counts: TypeCounts,  // all types
    pub tiles: u32, pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,   // first attempt's claimed_at
    pub updated_at: DateTime<Utc>,           // max task updated_at
}
pub struct TypeCounts { pub pending: u32, pub claimable_now: u32, pub claimed: u32, pub succeeded: u32, pub abandoned: u32 }
pub struct JobSummary {
    pub row: JobRow,
    pub by_type: Vec<(TaskType, TypeCounts)>,         // each type present in the job
    pub busy_by_type: Vec<(TaskType, u32)>,           // workers holding a lease of the type
    pub succeeded_durations_secs: Vec<(TaskType, Vec<u64>)>, // for the median; Task 6 derives ETA
    pub as_of: DateTime<Utc>,
}
pub struct TileStates { pub tile: TileId, pub ortho: Option<TaskState>, pub overlay: Option<TaskState> }
pub struct TaskQuery {
    pub status: Option<TaskState>, pub task_type: Option<TaskType>,
    pub worker: Option<String>, pub tile_prefix: Option<String>,
    pub search: Option<String>, pub cursor: Option<String>, pub limit: u32,   // limit 1..=500
}
pub struct TaskRow {
    pub task_id: TaskId, pub tile: TileId, pub task_type: TaskType, pub state: TaskState,
    pub attempts: u32, pub max_attempts: u32, pub updated_at: DateTime<Utc>,
    pub claimable_at: DateTime<Utc>,
    pub worker: Option<String>,               // the latest attempt's
    pub last_error: Option<String>,           // tasks.last_failure
    pub previous_error: Option<(u32, String)>,// (attempt_no, reason) of the latest earlier ended attempt with a reason, when the task is not abandoned
    pub last_heartbeat_at: Option<DateTime<Utc>>,
}
pub struct AttemptRow {
    pub attempt_id: Uuid, pub task_id: TaskId, pub job_id: JobId, pub tile: TileId,
    pub task_type: TaskType, pub worker: String, pub attempt_no: u32,
    pub claimed_at: DateTime<Utc>, pub ended_at: Option<DateTime<Utc>>,
    pub outcome: Option<AttemptOutcome>, pub reason: Option<String>,
    pub delivered_bytes: Option<u64>, pub log_tail: Option<String>,
}
pub struct WorkersSnapshot { pub as_of: DateTime<Utc>, pub workers: Vec<WorkerRow> }
pub struct WorkerRow {
    pub identity: String, pub first_seen_at: DateTime<Utc>, pub last_seen_at: DateTime<Utc>,
    pub task_types: Option<Vec<TaskType>>,
    pub lease: Option<LeaseView>,
    pub last_24h: Tallies,                    // over attempts claimed in (as_of − 24 h, as_of]
}
pub struct LeaseView {
    pub task_id: TaskId, pub job_id: JobId, pub region_code: String, pub revision: u32,
    pub tile: TileId, pub task_type: TaskType, pub attempt_no: u32, pub max_attempts: u32,
    pub claimed_at: DateTime<Utc>, pub last_heartbeat_at: DateTime<Utc>,
}
pub struct Tallies { pub succeeded: u32, pub failed: u32, pub reclaimed: u32 }
pub struct WorkerAttemptsQuery { pub identity: String, pub since: Option<DateTime<Utc>>, pub cursor: Option<String>, pub limit: u32 }
```

  - Exact derives and `Debug`/`Clone`/`PartialEq` are the implementer's call, consistent with `request.rs`. Cursors are opaque strings the adapter encodes; callers never parse them.
  - **Order:** `list_jobs` newest `created_at` first, then `id`. `list_tasks` by `updated_at DESC, id`. `worker_attempts` and `task_attempts` newest `claimed_at` first.
  - **Search:** `search` matches a tile prefix, a task-id prefix (at least 4 hex characters) or `last_failure` text, case-insensitively.
  - **`max_attempts`** in `TaskRow` and `LeaseView` is the job's.
  - **`as_of`** is the adapter's clock, so derivation never disagrees with the store about now.

- `conformance::Subject` changes to:

  ```rust
  pub struct Subject {
      pub store: Arc<dyn TaskStore>,
      pub queries: Arc<dyn OperatorQueries>,
      pub clock: Arc<TestClock>,
  }
  ```

  Both handles share one backing store. Update both existing fixtures; the in-memory one wraps one `Arc<InMemoryTaskStore>` as both.
- New macro `operator_conformance_suite!` and parity test `all_operator_cases_are_registered`, mirroring the existing pair. Cases build their fixtures **only by driving `TaskStore`** (create, claim, heartbeat, complete, fail, reap) and advancing `TestClock`, never by writing state directly.

- [ ] **Step 1: Write the failing operator conformance cases**, one per behaviour:
  - `a_claim_opens_an_attempt_and_complete_closes_it_with_bytes_and_tail`
  - `a_failed_attempt_keeps_its_reason_and_tail`
  - `a_reaped_attempt_records_the_lapse_and_writes_last_failure` (both reasons, from the shared definitions)
  - `a_claim_that_finds_nothing_still_records_the_worker`
  - `a_heartbeat_refreshes_last_seen`
  - `a_workers_lease_is_visible_while_held_and_gone_after_complete`
  - `tallies_count_only_the_last_24_hours`
  - `list_jobs_pages_newest_first`
  - `a_jobs_counts_and_started_at_follow_its_attempts`
  - `job_spec_returns_the_stored_toml_and_none_for_an_empty_one`
  - `tile_states_pair_ortho_with_overlay_and_none_without_overlays`
  - `list_tasks_filters_by_status_type_worker_and_tile`
  - `list_tasks_search_matches_tile_task_id_and_error_text`
  - `list_tasks_pages_by_updated_at_without_gaps_or_repeats`
  - `a_task_row_names_its_latest_worker_and_previous_error`
  - `worker_attempts_page_newest_first_and_honour_since`
  - `summary_reports_busy_workers_and_succeeded_durations_by_type`
  - `ready_is_ok_for_a_healthy_store`
  - `an_unknown_job_is_refused_by_every_job_read` (`NoSuchJob`; `task_attempts` of an unknown task answers an empty list)
- [ ] **Step 2: Run** `cargo test -p oxo-tasks --all-features`. Expected: compile failure (the port does not exist yet).
- [ ] **Step 3: Implement** the port and the in-memory adapter's `OperatorQueries`.
- [ ] **Step 4: Run** `cargo test --workspace --exclude oxo-tasks-postgres --all-features`. Expected: green. `oxo-tasks-postgres` does not compile (its fixture lacks `queries`), as the commit states.
- [ ] **Step 5: Commit** `feat(tasks): the OperatorQueries read port (port + in-memory)`.

### Task 4: The read port on PostgreSQL

**Files:**
- Create: `oxo-tasks-postgres/src/queries.rs`, `oxo-tasks-postgres/tests/operator_conformance_postgres.rs`
- Modify: `oxo-tasks-postgres/src/lib.rs`, `oxo-tasks-postgres/tests/conformance_postgres.rs` (`Subject.queries`)

**Interfaces:**
- Consumes: Task 3's port.
- Produces: `impl OperatorQueries for PostgresTaskStore`.
  - `ready()` runs `SELECT 1` and checks that the `_sqlx_migrations` table records every migration embedded in the binary, successful. A pending or failed migration is `TaskStoreError::Adapter("schema not current: …")`.
  - Cursors are keyset positions, never `OFFSET`.
- [ ] **Step 1: RED** is Task 3's compile failure in `make verify-db`. Capture it.
- [ ] **Step 2: Implement.** One SQL query per method where practical. `tile_states` is one grouped query; `list_workers` joins the open attempt for the lease. Search uses `ILIKE` with `%`/`_` escaped in user input; task-id prefix uses `id::text LIKE $1 || '%'`.
- [ ] **Step 3: Run** `make verify-db`. Expected: both suites green.
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(tasks-postgres): OperatorQueries on PostgreSQL`.

### Task 5: The stored spec and the worker endpoints' new fields

**Files:**
- Modify: `oxo-spec/src/spec.rs` (canonical rendering), `oxo-spec-cli/src/main.rs` (`show` uses it), `oxo-control/src/planner.rs`, `oxo-control/src/api/wire.rs`, `oxo-control/src/api/tasks.rs`

**Interfaces:**
- Produces:
  - `RegionSpec::to_canonical_toml(&self) -> String`, the same rendering `oxo-spec show` prints today (`toml::to_string_pretty`); `show` calls it.
  - The planner fills `spec_toml` with the canonical TOML of a clone of the spec whose `failure_policy.alert_destinations` is **emptied**, and `alert_destinations` with the original list.
  - In `wire.rs`: `pub const LOG_TAIL_MAX_BYTES: usize = 65_536;`, plus the optional fields:
    - `CompleteBody { lease_token, #[serde(default)] delivered_bytes: Option<u64>, #[serde(default)] log_tail: Option<String> }` (`CompleteBody` replaces the shared `LeaseBody` on the complete route only)
    - `FailBody` gains `#[serde(default)] log_tail: Option<String>`
  - A `log_tail` longer than `LOG_TAIL_MAX_BYTES` bytes is answered `413` with error code `log_tail_too_large`, through the one error mapping.
- [ ] **Step 1: Write the failing tests:**
  - `planner::tests::the_stored_spec_omits_alert_destinations` (seed `https://hooks.example/abc123`; assert `spec_toml` lacks `abc123` and `alert_destinations` holds it)
  - `planner::tests::the_stored_spec_round_trips` (parsing `spec_toml` gives back the spec minus destinations)
  - router tests:
    - `complete_without_the_new_fields_still_works` (bite check)
    - `complete_carries_delivered_bytes_and_tail_to_the_store` (read back through `OperatorQueries`)
    - `fail_carries_a_tail`
    - `an_oversized_tail_is_413`
  - `oxo-spec-cli` `show` output unchanged (an existing test, run as a bite check)
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(control): store the spec without its secrets; complete and fail carry bytes and a log tail`.

### Task 6: Derivation — worker state, thresholds and ETA

**Files:**
- Create: `oxo-control/src/derive.rs`
- Modify: `oxo-control/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct Liveness { pub late_secs: u64, pub idle_secs: u64, pub gone_secs: u64, pub heartbeat_timeout_secs: u64 }`
  - `Liveness::new(...) -> Result<Self, LivenessError>`, enforcing the validity rule from the shared definitions; `LivenessError` names the violated relation
  - `pub enum WorkerState { Working, Late, Reaping, Idle, Stale, GoneQuiet }` with `as_str` (`working`, `late`, `reaping`, `idle`, `stale`, `gone_quiet`) and `group()` (`busy`, `idle` or `gone_quiet`)
  - `pub fn worker_state(row: &WorkerRow, as_of: DateTime<Utc>, liveness: &Liveness) -> WorkerState`
  - `pub fn eta_seconds(summary: &JobSummary) -> Option<u64>`
- [ ] **Step 1: Write the failing unit tests:**
  - a table-driven test covering every row of the derived-state table at, just below and just above each boundary (strictly greater-than is the rule)
  - `liveness_rejects_late_not_below_timeout`, `liveness_rejects_idle_not_below_gone`, `liveness_rejects_zero`
  - `eta_is_none_below_five_successes`, `eta_is_zero_when_nothing_remains`, `eta_takes_the_slowest_type`, `eta_divides_by_busy_workers_with_a_floor_of_one`, `eta_uses_the_median_not_the_mean` (with an outlier)
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(control): derive worker state and ETA from facts`.

### Task 7: The read endpoints

**Files:**
- Create: `oxo-control/src/api/reads.rs`, `oxo-control/src/api/csv.rs`
- Modify: `oxo-control/src/api/mod.rs`, `oxo-control/src/api/jobs.rs`, `oxo-control/src/api/wire.rs`, `oxo-control/src/api/error.rs`, `oxo-control/src/api/test_support.rs`, and every `router(` call site (`oxo-controld/src/main.rs`, `oxo-control/tests/acceptance.rs`, `oxo-worker/tests/client.rs`, `oxo-worker/tests/acceptance.rs`, `oxo-worker/tests/run.rs`)

**Interfaces:**
- Produces:
  - The router entry point becomes:

    ```rust
    pub struct ApiDeps {
        pub store: Arc<dyn TaskStore>,
        pub queries: Arc<dyn OperatorQueries>,
        pub liveness: Liveness,
    }
    pub fn router(deps: ApiDeps) -> Router;
    ```

    Test call sites build `ApiDeps` from one `Arc<InMemoryTaskStore>`. `test_support` gains a helper that does so with default liveness.
  - Routes, as in the 5a spec's API section (all `GET`, open):

    | Route | Response |
    |---|---|
    | `/api/v1/jobs` | **Lookup** when both `region_code` and `revision` are present: unchanged `FoundJobBody`, or `404`. **List** when neither is present: `{ items: [JobRowBody], next_cursor }` (query `cursor`, `limit` default 50, max 200). One of the two present is `400` (`bad_query`) |
    | `/api/v1/jobs/{id}/summary` | `JobSummaryBody`: the row fields, `by_type: { ortho: TypeCountsBody, overlay?: TypeCountsBody }`, `eta_seconds: u64 \| null`, `as_of`; with a strong `ETag` and `304` on a matching `If-None-Match` |
    | `/api/v1/jobs/{id}/spec` | `text/plain; charset=utf-8`; `404` (`no_spec`) when none is stored |
    | `/api/v1/jobs/{id}/tiles` | `{ "tiles": [[tile, ortho_state, overlay_state \| null], …] }`, sorted by tile, with a strong `ETag` (a hash of the body) and `If-None-Match` answered `304` |
    | `/api/v1/jobs/{id}/tasks` | query `status`, `type`, `worker`, `tile`, `q`, `cursor`, `limit` (default 100, max 500). `{ items: [TaskRowBody], next_cursor }`, or `Accept: text/csv` streams **every** matching row (following cursors internally) with header `tile,type,status,attempts,max_attempts,worker,updated_at,last_error` and RFC 4180 quoting |
    | `/api/v1/tasks/{id}/attempts` | `{ items: [AttemptBody] }` |
    | `/api/v1/workers` | `{ as_of, thresholds: {late_secs, idle_secs, gone_secs, heartbeat_timeout_secs}, counts: {busy, idle, stale, gone_quiet}, workers: [WorkerBody] }`, with `WorkerBody.state` derived by Task 6 and a strong `ETag`. `counts.idle` covers idle only, `counts.stale` stale only, so the console can show "2 idle · 1 stale" |
    | `/api/v1/workers/{identity}/attempts` | query `since`, `cursor`, `limit` (default 100, max 500) |

  - Times are RFC 3339 UTC strings; enums use their `as_str` labels; ids are UUID strings.
  - Unknown job: `404` `no_such_job`. A bad enum or limit in a query: `400` `bad_query`, naming the parameter.
- [ ] **Step 1: Write the failing router tests,** one or more per route:
  - shape, filters, paging (follow `next_cursor` to the end with no repeats), `304` on a matching `If-None-Match`
  - CSV header and quoting (an error text with a comma, a quote and a newline)
  - the preserved lookup, and `bad_query` cases
  - **`no_read_returns_an_alert_destination`**: submit a spec with destination `https://hooks.example/abc123`, drive one task through claim and fail, then fetch every route in this Task (including CSV) and assert no body contains `abc123`
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.** The handlers stay thin: query, then map to wire. All mapping lives in `wire.rs`.
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(control): the operator read endpoints`.

### Task 8: `oxo-controld` — thresholds, readiness and composition

**Files:**
- Modify: `oxo-controld/src/config.rs`, `oxo-controld/src/main.rs`, `oxo-control/src/api/mod.rs` (`/readyz`)

**Interfaces:**
- Produces:
  - Flags `--worker-late-secs` (`OXO_WORKER_LATE_SECS`, 60), `--worker-idle-secs` (`OXO_WORKER_IDLE_SECS`, 120), `--worker-gone-secs` (`OXO_WORKER_GONE_SECS`, 600)
  - `Config::liveness() -> Result<Liveness, LivenessError>`, using `heartbeat_timeout_secs`; an invalid combination exits with code 2 and the error's message
  - `GET /readyz` in the router: `200` with body `ready` when `queries.ready()` is `Ok`, else `503` with the error text. `/healthz` stays as it is (`200`, no checks)
  - `main` builds one `Arc<PostgresTaskStore>` and passes it as both `store` and `queries`
- [ ] **Step 1: Write the failing tests:**
  - `config` tests for the three flags' defaults and environment variables
  - `an_invalid_liveness_combination_is_an_error`
  - router tests `readyz_is_200_when_ready`, and `readyz_is_503_with_the_reason_when_not` (with a test `OperatorQueries` double whose `ready` fails, delegating the rest to the in-memory store)
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify`, then `make verify-db`. Expected: green.
- [ ] **Step 5: Commit** `feat(controld): liveness thresholds and readiness`.

### Task 9: Worker identity per boot

**Files:**
- Modify: `oxo-worker/src/config.rs`, `oxo-worker/src/run.rs` (startup log), `oxo-worker/Cargo.toml` if a random source is needed (prefer `uuid` v4's bytes, already a dependency)

**Interfaces:**
- Produces:
  - `pub fn boot_id() -> String`: 8 lowercase hex characters, random
  - `Config::worker_identity(&self, boot_id: &str) -> String`: `<name>-<boot_id>`, where name is `OXO_WORKER_NAME` (non-empty), else the trimmed kernel hostname (non-empty), else `boot_id` alone. The fixed `"oxo-worker"` fallback is removed.
  - `run` computes the identity **once** at startup, logs `worker identity: <identity>`, and sends it on every claim.
- [ ] **Step 1: Write the failing tests:**
  - `identity_appends_the_boot_id_to_the_configured_name`
  - `identity_appends_the_boot_id_to_the_hostname` (inject the hostname through a seam, `worker_identity_with(name, hostname, boot_id)`)
  - `identity_is_the_boot_id_alone_without_a_name`
  - `boot_ids_are_eight_hex_and_differ` (draw 100; all match `^[0-9a-f]{8}$`; no duplicates)
  - a run-loop test asserting every claim in one process carries the same identity
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(worker): a per-boot identity`.

### Task 10: Worker reports delivered bytes and a masked log tail

**Files:**
- Create: `oxo-worker/src/tail.rs` (the ring buffer and masking)
- Modify: `oxo-worker/src/runner.rs`, `oxo-worker/src/exec.rs`, `oxo-worker/src/api.rs`, `oxo-worker/src/run.rs`, `oxo-worker/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub const LOG_TAIL_MAX_BYTES: usize = 65_536;` in `tail.rs`. A test asserts it equals `oxo_control::api::wire::LOG_TAIL_MAX_BYTES` (a dev-dependency use, which is allowed).
  - `pub struct Tail`: a byte ring buffer keeping the **last** `LOG_TAIL_MAX_BYTES` bytes written; `push(&mut self, bytes: &[u8])`; `render(&self) -> String`, lossy UTF-8 with a leading partial character dropped, masked
  - `pub fn mask(text: &str) -> String`, replacing with `***`:
    - the value of any URL query parameter whose name, case-insensitively, is `key`, `token`, `access_token`, `apikey` or `api_key` (the value runs to the next `&`, `#`, whitespace or quote);
    - the rest of a line after `Authorization:` (case-insensitive);
    - any `pk.` or `sk.` token followed by at least 20 characters of `[A-Za-z0-9._-]` (Mapbox-shaped).

    Masking is applied once, at render.
  - **Runner output.** The runner's **stderr** changes from `Stdio::inherit()` to piped. A drain task copies every chunk to the worker's own stderr unchanged, so pod logs are unaffected, and pushes it into a `Tail`. Stdout keeps its current contract (the last JSON line is the result). Its non-result lines are pushed into the same `Tail` too, so the tail reflects the combined output in arrival order per stream. `TaskRun::wait` returns the outcome and the rendered tail.
  - `exec::egress` returns `Result<Delivered, EgressError>`, where `pub struct Delivered { pub bytes: u64 }` is the total size of the regular files in the committed deliverable.
  - `api::ControlPlane::complete(task_id, lease, delivered_bytes: Option<u64>, log_tail: Option<&str>)` and `fail(..., log_tail: Option<&str>)` send the new optional fields.
  - The loop sends `delivered_bytes` and the tail on success. On failure (runner failure or egress failure) it sends the tail. A lease lost mid-build sends nothing, as today.
- [ ] **Step 1: Write the failing tests:**
  - `tail_keeps_the_last_bytes_only`
  - `tail_drops_a_leading_partial_character`
  - one masking test per rule:
    - `mask_hides_query_keys_case_insensitively`
    - `mask_hides_authorization_headers`
    - `mask_hides_mapbox_shaped_tokens`
  - **`mask_does_not_catch_a_key_in_a_url_path`**: assert that `https://tiles.example/k/SECRET123/1/2/3.jpg` is **unchanged**. This documents the limitation and must pass.
  - `the_cap_matches_the_control_planes`
  - `egress_reports_the_delivered_bytes` (sum over a fixture tree)
  - runner-contract tests:
    - `stderr_still_reaches_the_workers_stderr_and_the_tail`
    - `a_runner_that_writes_more_than_the_cap_yields_a_capped_tail`
  - run-loop tests (in-process router):
    - `a_success_reports_bytes_and_a_tail`
    - `a_failure_reports_a_tail`
    - `an_egress_failure_reports_a_tail`
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify`. Expected: green. Also run `make worker-smoke`, since the runner's stderr handling changed and the image's direct execution must still pass.
- [ ] **Step 5: Commit** `feat(worker): report delivered bytes and a masked log tail`.

### Task 11: `oxo-console` — the crate, configuration, client, layout and styles

**Files:**
- Create: `oxo-console/Cargo.toml`, `oxo-console/src/main.rs`, `oxo-console/src/lib.rs`, `oxo-console/src/config.rs`, `oxo-console/src/client.rs`, `oxo-console/src/app.rs`, `oxo-console/src/error.rs`, `oxo-console/templates/base.html`, `oxo-console/templates/partials/header.html`, `oxo-console/templates/unreachable.html`, `oxo-console/static/console.css`, `oxo-console/static/fonts/` (`InterVariable.woff2`, `InterVariable-Italic.woff2`, `JetBrainsMono[wght].woff2` or the static `JetBrainsMono-Regular.woff2` and `-Bold.woff2`, plus both `OFL.txt` licence files)
- Modify: `Cargo.toml` (workspace member; `askama` added to workspace dependencies), `Makefile` (no change needed for `verify`, which runs the workspace)

**Interfaces:**
- Produces:
  - **Configuration** (`clap` derive with env):
    - `--controld-url` (`OXO_CONTROLD_URL`, required)
    - `--bind` (`OXO_CONSOLE_BIND`, `127.0.0.1:8090`)
    - `--mapbox-token` (`OXO_MAPBOX_TOKEN`, optional; empty counts as unset)
    - `--mapbox-style` (`OXO_MAPBOX_STYLE`, `mapbox://styles/mapbox/light-v11`)

    `Config::validate()` refuses a token starting with `sk.` (exit code 2, message: `OXO_MAPBOX_TOKEN must be a Mapbox public token (pk.…); secret tokens (sk.…) must never reach a browser`).
  - **`client::Controld`**, a typed reqwest client over the read endpoints, returning `oxo_control::api::wire` bodies:
    - `Err(Unreachable)` on connect failures and timeouts (5 s)
    - `Err(Status(code, body))` on non-2xx
    - it forwards and returns `ETag`s for the polled routes
  - **`app::router(state) -> Router`**, with `/static/*` served from files embedded in the binary (`include_bytes!` or a small embed macro; no runtime file access) with long-lived cache headers, and `/api/*` proxied to `oxo-controld` for **`GET` only**. Other methods answer `405` in 5a.
  - **`base.html`**:
    - landmarks (`header`, `nav`, `main`)
    - the OXO mark and "console" wordmark
    - nav links Jobs and Workers, with `aria-current` on the current page
    - the header status line, the `partials/header.html` fragment: "controld ready · N busy · M idle", or "controld unreachable" / "controld not ready", from `/readyz` plus `/api/v1/workers` counts
    - a "Read-only" chip with no link (5a)
    - `<html lang="en">`
    - `{% block main %}`
  - **`unreachable.html`**: status `502`, an explanation, and a retry link to the same URL. Every page handler maps `Unreachable` to it.
  - **`console.css`**:
    - **Tokens:** Catppuccin Latte tokens on `:root`, and Mocha tokens under `@media (prefers-color-scheme: dark)`. Token names: `--base`, `--mantle`, `--crust`, `--surface0`, `--surface1`, `--overlay0`, `--text`, `--subtext1`, `--subtext0`, `--blue`, `--green`, `--red`, `--yellow`. Values come from <https://catppuccin.com/palette/>. Explicit `body { background: var(--base); color: var(--text); }`.
    - **Status markers:** `--status-claimed: var(--blue)`, `--status-succeeded: var(--green)`, `--status-abandoned: var(--red)`, `--status-late: var(--yellow)`, `--status-pending: var(--overlay0)`. Markers, fills and bars only; **text always uses `--text` or `--subtext1`**; links are underlined `--text`.
    - **Fonts:** `@font-face` for Inter and JetBrains Mono from `/static/fonts/`. `--font-ui: "Inter", system-ui, sans-serif` and `--font-mono: "JetBrains Mono", ui-monospace, monospace`.
    - **Focus:** visible `:focus-visible` rings.
- [ ] **Step 1: Write the failing tests:**
  - `config_refuses_a_secret_mapbox_token`, `config_treats_an_empty_token_as_unset`, and the defaults
  - `client` tests against a fake `oxo-controld` (an axum router in-process): a success body, `Unreachable` when nothing listens, `Status` on `404`
  - `base_renders_landmarks_and_one_h1_slot`
  - `the_header_reports_ready_and_counts`, `the_header_reports_unreachable`
  - `a_page_with_controld_down_is_502_with_a_retry_link`
  - `the_proxy_forwards_get_and_refuses_post`
  - `static_assets_are_served_with_cache_headers`
  - **`text_tokens_meet_contrast_in_both_themes`**: parse `console.css`, resolve `--text` and `--subtext1` against `--base`, `--mantle` and `--surface0` for Latte and Mocha, and assert at least 4.5:1 with the WCAG relative-luminance formula
- [ ] **Step 2: Fetch the fonts** from the projects' official releases (Inter from `github.com/rsms/inter` releases, JetBrains Mono from `github.com/JetBrains/JetBrainsMono` releases). Record each file's release version and SHA-256 in `oxo-console/static/fonts/README.md`, and keep each `OFL.txt`.
- [ ] **Step 3: Run the tests.** Expected: they fail.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run** `make verify`. Expected: green.
- [ ] **Step 6: Commit** `feat(console): the oxo-console crate, layout, themes and fonts`.

### Task 12: Dashboard

**Files:**
- Create: `oxo-console/src/pages/mod.rs`, `oxo-console/src/pages/dashboard.rs`, `oxo-console/templates/dashboard.html`, `oxo-console/templates/partials/job_rows.html`
- Modify: `oxo-console/src/app.rs`

**Interfaces:**
- Produces `GET /`, matching screen 08 minus actions:
  - Heading "Regional scenery jobs" and the subtitle "Each job is one region; each task is one 1×1° ortho or overlay tile."
  - Two tables: **In progress** (status `in_progress`) and **Finished** (`complete` or `failed`), each with a count in its heading.
  - Columns: Region (name, then `REGION · rev N` in mono; a job with no name shows its region code as the name), Status (a marker plus text), Progress (a bar plus "P% · S / T tasks", plus "· A abandoned" when A > 0), Tiles, Last update (relative under 24 h, "Mon D, HH:MM" otherwise, with the exact time in a `<time datetime>` element).
  - No Actions column in 5a.
  - Each row links to `/jobs/{id}` through its region name.
  - `GET /frag/jobs` renders the two tables' bodies for the poller, wrapped in `data-poll` regions.
  - It pages through `/api/v1/jobs` until exhausted (jobs are few). If there are more than 200, a "Showing the newest 200" note appears.
- [ ] **Step 1: Write the failing tests:** rendering from a fixture list covering every status (`dashboard_splits_in_progress_and_finished`, `progress_text_includes_abandoned_only_when_present`, `a_nameless_job_shows_its_region_code`, `times_render_relative_and_absolute`, `the_fragment_matches_the_page_section` (byte-equal to the page's corresponding section), `no_action_buttons_in_5a`).
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(console): the dashboard`.

### Task 13: Job detail — summary, tile grid, tile panel and spec

**Files:**
- Create: `oxo-console/src/pages/job.rs`, `oxo-console/templates/job.html`, `oxo-console/templates/partials/job_summary.html`, `oxo-console/templates/partials/tile_grid.html`, `oxo-console/templates/partials/tile_panel.html`, `oxo-console/templates/spec.html`

**Interfaces:**
- Produces:
  - **`GET /jobs/{id}`** (map view, the default):
    - a breadcrumb `Jobs / <name>`
    - an `h1` with the name, `REGION · rev N` and the status
    - a "View spec (TOML)" link, shown only when a spec is stored
    - the summary block, `partials/job_summary.html`, matching screen 02:
      - the percentage, and "S / T tasks complete"
      - "Started <time> (first claim)", "ETA ~ <human>" (or no ETA text when null), and "updated <relative>"
      - a progress bar
      - a legend line: "Ortho s / t", "Overlay s / t" (only when the job has overlays), "Claimed n", "Pending n", "Abandoned n"
    - the view switch, Map | List: links to `?view=list`, with `aria-current`
    - the tile area, which contains the **keyboard grid** `partials/tile_grid.html`. With a Mapbox token it also contains `<div id="map" data-tiles-url="/api/v1/jobs/{id}/tiles" data-token=… data-style=…>`. Without one it shows the note `Map not configured: set OXO_MAPBOX_TOKEN`.
  - **The grid** is a `<table role="grid">`, one row per latitude, rows ordered from north to south, cells ordered west to east. Each tile cell is a link to `?tile=<tile>` whose text is the tile and whose accessible name is `<tile>: ortho <state>, overlay <state|none>`. Cell markers use the status tokens, with a pattern class for abandoned. Gaps where the job has no tile are empty `<td>`s. Columns are headed by longitude and rows by latitude.
  - **`?tile=<tile>`** renders `partials/tile_panel.html` as a focus-target `<aside aria-labelledby>`:
    - the tile code in mono, "a°–b° N · c°–d° W · n tasks", and a close link back to the URL without `tile`
    - per task: the state; task id (first 8 characters, mono); worker; "Attempts n of max"; last update (absolute, plus relative)
    - on success: "built in <duration>" (the latest attempt's `ended_at − claimed_at`), and "Deliverable <human bytes> → <target root>" (the target root from the stored spec, when present)
    - the last error, in a bordered block
    - "View log", a `<details>` element containing the latest attempt's `log_tail` in a `<pre>`, or "No log reported" when it is none
  - The tile panel is fetched from `/api/v1/jobs/{id}/tasks?tile=<tile>` and `/api/v1/tasks/{task}/attempts`. **No Retry button in 5a.**
  - **`GET /jobs/{id}/spec`** renders the TOML in a `<pre>` (mono) with a "Download" link to `/api/v1/jobs/{id}/spec`, or `404` with "No spec was stored for this job (it predates the console)".
  - Fragments: `GET /frag/job/{id}/summary` and `GET /frag/job/{id}/tiles` (the grid).
- [ ] **Step 1: Write the failing tests:**
  - `the_summary_matches_screen_02s_fields`
  - `overlay_counts_hide_for_a_job_without_overlays`
  - `eta_text_is_absent_when_null`
  - `the_grid_orders_north_to_south_west_to_east_with_gaps`
  - `every_grid_cell_has_an_accessible_name_with_both_states`
  - `the_map_div_appears_only_with_a_token`
  - `without_a_token_the_page_says_how_to_configure_the_map`
  - `the_tile_panel_shows_bytes_duration_error_and_log`
  - `a_missing_log_says_so`
  - `the_spec_page_404s_for_a_pre_5a_job`
  - `fragments_match_their_page_sections`
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(console): job detail — summary, tile grid, tile panel and spec`.

### Task 14: Job detail — the list view

**Files:**
- Create: `oxo-console/templates/partials/task_rows.html`
- Modify: `oxo-console/src/pages/job.rs`, `oxo-console/templates/job.html`

**Interfaces:**
- Produces `GET /jobs/{id}?view=list&status=&type=&worker=&q=&cursor=`, matching screen 06 minus selection, retry and superseded:
  - **Filters:** a plain `<form method="get">` with selects Type (All, Ortho, Overlay), Status (All, Pending, Claimed, Succeeded, Abandoned) and Worker (Any, plus the workers seen in this job), a search box ("Search tile, task id or error…") and an Apply button. Every control has a `<label>`.
  - **Pinning:** when no status filter is set, abandoned tasks are fetched first (`status=abandoned`, all pages) and rendered under a "ABANDONED · n (pinned)" group heading, then "ALL OTHER TASKS · n" from the unfiltered list, excluding abandoned.
  - **Columns:** Tile (mono), Type, Status (marker plus text), Worker (mono; `—` when none), Attempts "n/max" (the text stays `--text`; when n = max the marker is red), Last update ("HH:MM:SS · relative", or "queued <relative>" for pending), Last error (truncated with the full text in `title`; or "prev: <reason> (attempt k)" from `previous_error`; or `—`).
  - **Paging:** "Showing n of t tasks", and a "Next" link carrying `cursor`.
  - **Export:** an "Export CSV" link to `/api/v1/jobs/{id}/tasks?…same filters…` with `Accept` handled by a `?format=csv` alias on the console proxy (browser links cannot set `Accept`; the proxy maps `format=csv` to the header).
  - Fragment: `GET /frag/job/{id}/tasks?…` (the rows).
- [ ] **Step 1: Write the failing tests:**
  - `filters_round_trip_through_the_url`
  - `abandoned_pins_above_the_rest_without_a_status_filter`
  - `a_status_filter_disables_pinning`
  - `previous_error_renders_as_prev`
  - `pending_rows_say_queued`
  - `the_next_link_carries_the_cursor`
  - `export_csv_link_carries_the_filters`
  - `the_proxy_maps_format_csv_to_accept` (the proxy test)
  - `every_filter_control_is_labelled`
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(console): job detail — the task list`.

### Task 15: Workers

**Files:**
- Create: `oxo-console/src/pages/workers.rs`, `oxo-console/templates/workers.html`, `oxo-console/templates/partials/worker_rows.html`, `oxo-console/templates/partials/worker_panel.html`

**Interfaces:**
- Produces `GET /workers`, matching screen 07:
  - **Heading and intro:** the heading "Workers" and the intro "Workers pull work from oxo-controld; OXO cannot start, stop or scale them. State is inferred from claims (including empty ones) and heartbeats; lease health is shown on the lease each worker holds."
  - **Cards:**
    - Busy n ("holding a lease · k late")
    - Idle n ("no lease · k stale (2–10 min)", using the thresholds)
    - Gone quiet n ("not seen for > 10 min")
    - Reclaimed (24 h) n ("leases expired by the reaper")
    - Attempts succeeded (24 h) n ("of t · p %")
  - **Table**, grouped BUSY, IDLE and GONE QUIET, each with a count. Columns:
    - Worker (mono, middle-ellipsised to 24 characters, with the full identity in `title` and a copy button)
    - State (marker plus text)
    - Current task ("+48-110 Ortho · attempt 1", then `REGION · rev N`; for idle, "Last claim: overlay only" when the filter is `[overlay]`, or "No lease; not polled for <relative>" when stale; for gone quiet, its last attempt's outcome)
    - Heartbeat (a `<meter>` of heartbeat age against `heartbeat_timeout_secs`, with the seconds as text)
    - Running (since `claimed_at`)
    - Last seen
    - 24 h (✓ n, × n, ↻ n, each with a visually hidden word: "succeeded", "failed", "reclaimed")
  - **Footer:** "Identity is unique per process start (name + boot id). A restarted worker appears as a new worker; its predecessor fades to Gone quiet." and "Thresholds from oxo-controld: late Ns · lease timeout Ns · sweep — · idle N m · gone quiet N m". The sweep interval is not in the API; render `—` rather than invent it.
  - **`?worker=<identity>`** opens `partials/worker_panel.html`:
    - the identity in mono, the state, and "First seen … · last seen …"
    - a Late or Reaping explainer for those states, of the form "No heartbeat for N s. At T s the lease expires and the next reaper sweep re-queues <tile> as attempt k+1 of max. If this worker reports afterwards, it gets 409 and stops."
    - Current lease (Task, Job, Claimed, Last heartbeat, and a bar for running time against the maximum duration, shown only when known)
    - "Attempts · last 24 h", a 24-bucket hourly histogram as an `<svg role="img">` with an `aria-label` summarising the counts
    - the median ortho duration
    - Attempt history (the latest 10, with an "All n attempts" link paging through `?worker=…&cursor=`)
  - **Not in 5a:** the "History: last 24 h" selector is not rendered (fixed at 24 h).
  - Fragment: `GET /frag/workers`.
- [ ] **Step 1: Write the failing tests:**
  - `cards_count_busy_idle_and_gone_with_late_and_stale_subtotals`
  - `rows_group_by_busy_idle_gone`
  - `an_overlay_only_idle_worker_says_so`
  - `a_stale_worker_says_when_it_last_polled`
  - `the_heartbeat_meter_uses_the_timeout`
  - `the_late_explainer_states_the_consequence`
  - `the_histogram_has_24_buckets_and_an_accessible_summary`
  - `long_identities_ellipsise_with_the_full_name_in_title`
  - `the_footer_renders_thresholds_and_a_dash_for_the_sweep`
- [ ] **Step 2: Run them.** Expected: they fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify`. Expected: green.
- [ ] **Step 5: Commit** `feat(console): the workers page`.

### Task 16: The JavaScript islands and `make test-js`

**Files:**
- Create: `oxo-console/static/js/poll.js`, `oxo-console/static/js/grid.js`, `oxo-console/static/js/map.js`, `oxo-console/static/js/tests/poll.test.mjs`, `oxo-console/static/js/tests/grid.test.mjs`, `oxo-console/static/js/tests/map.test.mjs`
- Modify: `oxo-console/templates/base.html` (module script tags), `oxo-console/templates/job.html`, `Makefile` (`test-js`, and `verify` depends on it), `CONTRIBUTING.md` (Node as a dev prerequisite)

**Interfaces:**
- Produces:
  - **`poll.js`.** For each `[data-poll]` element with `data-poll-url`, every 10 s while `document.visibilityState === 'visible'`, it fetches with `If-None-Match`:
    - `200`: replaces `innerHTML` and stores the `ETag`
    - `304`: does nothing but refresh "updated" stamps
    - `502` or a network error: adds `data-stale` (the CSS shows a "stale, retrying" note) and backs off exponentially, up to 60 s

    It pauses while an `<aside>` panel is open (the URL has `tile=` or `worker=`) or `getSelection()` is non-empty. It updates every `<time data-relative>` once a second. The logic is exported as pure functions (`nextDelay`, `shouldPoll`, `relativeText`) so the tests need no DOM.
  - **`grid.js`.** Arrow keys move focus between grid cells (roving `tabindex`); Home and End go to the row's ends; Enter follows the cell's link. It exports a pure `move(position, key, shape)`.
  - **`map.js`.** Loaded only when `#map` exists, as an ES module. It:
    - injects Mapbox GL JS from `https://api.mapbox.com/mapbox-gl-js/v3.x/mapbox-gl.js` (and its CSS), pinned to an exact version recorded in the file;
    - sets `accessToken` from `data-token`, and builds the map with `data-style`;
    - fetches `data-tiles-url`;
    - builds two GeoJSON sources, ortho bodies (the full 1×1° square) and overlay strips (the southern 15 % of the square), with `fill-color` from the CSS status tokens read via `getComputedStyle`, and a `fill-pattern` for abandoned;
    - provides the layer toggle (Ortho + Overlay, Ortho, Overlay), a status filter, and go-to-tile (`flyTo`);
    - on tile click, navigates to `?tile=<tile>`;
    - fits the bounds of the job's tiles on load.

    It exports a pure `tileFeatures(tiles)` that converts the API's tile array to features (tested), keeping the Mapbox calls thin.
  - **Makefile:** `make test-js` runs `node --test oxo-console/static/js/tests/`, and `verify` gains `test-js`. It requires Node 20 or newer; the target fails with a clear message when `node` is absent.
- [ ] **Step 1: Write the failing Node tests** for `nextDelay`, `shouldPoll`, `relativeText`, `move`, and `tileFeatures` (polygon corners for a northern-and-western tile and a southern-and-eastern one, and the strip geometry; no overlay feature when the overlay state is null).
- [ ] **Step 2: Run** `make test-js`. Expected: it fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `make verify` (which now includes `test-js`). Expected: green.
- [ ] **Step 5: Commit** `feat(console): polling, keyboard grid and Mapbox map islands`.

### Task 17: Gherkin acceptance

**Files:**
- Create: `oxo-control/features/operator_reads.feature`, `oxo-worker/features/identity.feature`, `oxo-console/features/console.feature`, `oxo-console/tests/acceptance.rs`
- Modify: `oxo-control/tests/acceptance.rs`, `oxo-worker/tests/acceptance.rs`, `oxo-console/Cargo.toml` (dev-dependencies: `cucumber`, `oxo-control`, `oxo-tasks`)

**Interfaces:**
- Produces the scenarios in the 5a spec's "Gherkin features" section, verbatim apart from step-wording adjustments the step definitions need:
  - **Operator reads:** the reaped attempt, the idle worker, the finished task's builder and bytes, the secret guard, the old lookup.
  - **Worker identity:** two pods differ; a restart is a new worker.
  - **Read-only console:**
    - Every page works without JavaScript, which the harness checks by fetching HTML with no script execution and asserting state text.
    - The map degrades without a token.
    - A secret token is refused.
    - With `oxo-controld` unreachable, the page renders `502`.

  The console scenarios run `oxo-console`'s router in-process against a real `oxo-control` router over the in-memory store, bound to an ephemeral port.
- [ ] **Step 1: Write the features.** Run them; the steps are undefined, so they fail.
- [ ] **Step 2: Implement the step definitions.**
- [ ] **Step 3: Run** `make verify`. Expected: green, with the scenario counts reported.
- [ ] **Step 4: Commit** `test: phase 5a acceptance scenarios`.

### Task 18: Documentation true-up

**Files:**
- Modify:
  - `docs/specs/2026-10-01-job-server-design.md`: the `OperatorQueries` port, the attempts and workers tables, `CompleteRequest`, the reaper's `last_failure`
  - `docs/specs/2026-10-02-control-plane-design.md`: the read and operational endpoints, the threshold flags, `/readyz`
  - `docs/specs/2026-10-02-worker-pod-design.md`: the per-boot identity, the log tail and its masking, delivered bytes
  - `docs/specs/2026-10-06-read-only-console-design.md`: a closing "As built" note recording any deviation
  - `CLAUDE.md`: the `oxo-console` crate row; `make test-js`; the console's run command; test counts transcribed from fresh `make verify` and `make verify-db` runs
  - `README.md`: Getting Started gains "Run the console", and the Documentation table gains the 5a design
  - `deploy/worker-pod.yaml`: a comment on `OXO_WORKER_NAME` describing the per-boot suffix
  - `worker/README.md`: the log tail and masking
- [ ] **Step 1: Make the edits.** Run `make verify` and `make verify-db`, and transcribe the counts from their output (name the suites; do not derive).
- [ ] **Step 2: Commit** `docs: phase 5a amendments and true-up`.

### Task 19: The real-run gate (controller, not a subagent)

Done by the controller with the operator, because it needs the operator's Mapbox token and live services.

- [ ] **Step 1:** Start `oxo-postgres` (`podman start oxo-postgres`) and `oxo-controld` from this branch (migration `0003` applies on boot).
- [ ] **Step 2:** Rebuild the worker image (`make image`), then start the maison worker pod.
- [ ] **Step 3:** Submit a fresh revision of a small real region. Use the PNW tile at revision 3 unless the operator names another.
- [ ] **Step 4:** Ask the operator for the Mapbox public token, then start `oxo-console` with `OXO_MAPBOX_TOKEN` taken from the environment. **Never write the token to a file in the repository, and never echo it.**
- [ ] **Step 5:** With the operator, confirm each screen against true data:
  - Dashboard: the job is In progress, then Finished.
  - Job detail map: tiles drawn on Mapbox, the tile panel showing the worker, delivered bytes, build time and the log tail.
  - List view: filters and CSV export.
  - Workers: the pod as Working, then Idle, with its per-boot identity.
  - Restart the console without the token and confirm the keyboard-grid fallback.
- [ ] **Step 6:** Record the outcome in the 5a design's "As built" note, open the Forgejo PR, and report.

---

## Self-review

- **Spec coverage.** Every section of the 5a design maps to a Task:
  - scope and no actions: Tasks 12–15
  - worker identity: Task 9
  - the data model: Tasks 1–2
  - the task store changes: Tasks 1–2 and 5
  - the read port: Tasks 3–4
  - worker changes: Tasks 9–10
  - the API: Tasks 5, 7 and 8
  - `oxo-console` (configuration, routes, islands, grid, Mapbox, look and feel, unreachable, accessibility): Tasks 11–16
  - testing: every Task, plus Task 17
  - the real-run gate: Task 19
  - amendments: Task 18
  - the 5d hand-forward is recorded in the spec and needs no 5a Task
- **Known judgement calls left to implementers,** each bounded by the interfaces above:
  - exact derives on the port types
  - cursor encoding
  - how the reaper's SQL reports lapse kinds
  - the embedding mechanism for static files
  - the Mapbox GL JS minor version pinned in `map.js`
- **Type consistency.** These names are used identically everywhere they appear:
  - `CompleteRequest`
  - `AttemptOutcome`
  - `OperatorQueries` and its row types
  - `Liveness`
  - `WorkerState`
  - `ApiDeps`
  - `LOG_TAIL_MAX_BYTES`
  - `Delivered`
  - `Tail`
