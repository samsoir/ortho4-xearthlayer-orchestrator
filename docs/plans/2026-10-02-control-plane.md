# Control Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The planner that atomizes a region specification into per-tile tasks, and the HTTP API that workers claim and report through — plus the port amendments the job server's final review settled for this sub-project.

**Architecture:** Two new crates. `oxo-control` is a library: the planner, an `axum::Router` over an injected `Arc<dyn TaskStore>`, the wire types, one error mapping, and the reaper loop — it never depends on `sqlx` or `oxo-tasks-postgres`. `oxo-controld` is the binary composition root: configuration, the PostgreSQL adapter, the server, the spawned reaper. Before either exists, the port in `oxo-tasks` is amended: checked whole-second and attempt types replace `std::time::Duration` and raw `u32`, `find_job` joins the trait, the reclaim contract is documented honestly, and the conformance suite grows the cases the final review named.

**Tech Stack:** Rust (edition 2021), `axum` 0.8 (raises the workspace `rust-version` floor to 1.75), `tower`/`http-body-util` for in-process handler tests, `serde`/`serde_json` for the wire, `tracing` for logs, `cucumber` 0.20.2 for acceptance, existing `oxo-spec`/`oxo-tasks`/`oxo-tasks-postgres` underneath.

**Spec:** `docs/specs/2026-10-02-control-plane-design.md` (building on `docs/specs/2026-10-01-job-server-design.md`, whose "Settled for sub-project 3, not open" section this plan's Tasks 1–5 implement, and `docs/specs/2026-10-01-oxo-architecture-design.md`).

## Global Constraints

Every task's requirements implicitly include this section.

- **TDD is mandatory.** Write the failing test, run it and capture the real failing output, then write the minimal code to pass. A report without per-cycle RED output is incomplete. Where a test asserts behaviour that already ships (several conformance cases below), the RED step is replaced by a **bite check**: run the new test once with a deliberately wrong expected value, show it fail, then restore the true expectation. A test never observed failing proves nothing.
- **`make verify` before every commit.** Format-check, clippy `-D warnings` (`--all-features`), tests with `RUSTFLAGS="-D warnings"`. Tasks that touch `oxo-tasks-postgres` must also run `make verify-db` and show it green before committing — except where a task below explicitly states `verify-db` is expected red at its end.
- **`oxo-control` must not depend on `sqlx`, `oxo-tasks-postgres`, or any database driver.** Only `oxo-controld` may. `oxo-tasks` additionally must not gain `serde`, `axum`, or any network crate — the wire types live in `oxo-control`.
- **Every timestamp comes from an injected `Clock`.** No `SystemTime::now()` or `Utc::now()` outside `SystemClock`; no SQL `now()`; no wall-clock sleeps in tests — handler tests use `tower::ServiceExt::oneshot`, reaper tests use `start_paused` tokio time plus `TestClock::advance`.
- **Every port method takes an owned request and returns an owned response**, no transaction spanning a call. `find_job` follows the pattern: it takes a `FindJob` request struct.
- **The worker protocol rule:** any `409` on a task report means the worker has lost that task and must stop. `lease_lost` and `not_claimed` are distinguished in the body for operators, never for workers to branch on. `503` is the only status a worker retries.
- **Commit messages end with** `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`, preceded by a blank line or git parses no trailer. Verify with `git log -1 --format='%(trailers)'`.
- **After committing, confirm what you committed.** `git status --porcelain` must be empty and `git show HEAD:<path>` must contain your change.
- **Do not state derived counts in prose** (test totals, case totals) anywhere a test does not enforce them; name the things instead. The conformance parity test is the one enforced count.

### Vocabulary

As in the job-server plan: a **job** is one submission of one specification revision, identified by `(region_code, revision)`; a **task** is one tile's ortho or overlay conversion within a job. This plan's own numbered units are capitalised **Tasks**. The **wire** is the JSON/TOML HTTP contract owned by `oxo-control`; the **port** is the `TaskStore` trait owned by `oxo-tasks`.

---

### Task 1: Checked quantities in the port

**Files:**
- Create: `oxo-tasks/src/quantity.rs`
- Modify: `oxo-tasks/src/lib.rs`

**Interfaces:**
- Produces: `BackoffSeconds` (`new(u64) -> Result<Self, InvalidQuantity>`, `const ZERO`, `get() -> u64`), `TimeoutSeconds` (`new(u64) -> Result<Self, InvalidQuantity>`, `get() -> u64`), `MaxAttempts` (`new(u32) -> Result<Self, InvalidQuantity>`, `get() -> u32`), `InvalidQuantity` (`Zero`, `TooManySeconds(u64)`), `MAX_SECONDS: u64`. All three quantity types are `Debug + Clone + Copy + PartialEq + Eq + PartialOrd + Ord + Hash`.
- Consumes: nothing new. Task 2 threads these through the request types.

- [ ] **Step 1: Write the failing tests**

Create `oxo-tasks/src/quantity.rs` containing only the test module first (the types referenced do not exist yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_backoff_is_legal_and_means_retry_immediately() {
        // oxo-spec defaults backoff_seconds to 0; refusing it here would
        // make the default specification unsubmittable.
        assert_eq!(BackoffSeconds::new(0), Ok(BackoffSeconds::ZERO));
        assert_eq!(BackoffSeconds::ZERO.get(), 0);
    }

    #[test]
    fn a_zero_timeout_is_refused() {
        // A zero heartbeat timeout or max duration reclaims every in-flight
        // task on the next reap tick, which is never what an operator meant.
        assert_eq!(TimeoutSeconds::new(0), Err(InvalidQuantity::Zero));
    }

    #[test]
    fn seconds_beyond_what_every_adapter_can_represent_are_refused() {
        assert!(BackoffSeconds::new(MAX_SECONDS).is_ok());
        assert_eq!(
            BackoffSeconds::new(MAX_SECONDS + 1),
            Err(InvalidQuantity::TooManySeconds(MAX_SECONDS + 1))
        );
        assert!(TimeoutSeconds::new(MAX_SECONDS).is_ok());
        assert_eq!(
            TimeoutSeconds::new(MAX_SECONDS + 1),
            Err(InvalidQuantity::TooManySeconds(MAX_SECONDS + 1))
        );
    }

    #[test]
    fn the_bound_converts_infallibly_into_chrono_arithmetic() {
        // The whole point of MAX_SECONDS: chrono::Duration counts
        // milliseconds in an i64, so any accepted value converts without
        // the unwrap_or(zero) inversion this type exists to remove.
        let seconds = BackoffSeconds::new(MAX_SECONDS).expect("in range").get();
        let converted = chrono::Duration::try_seconds(
            i64::try_from(seconds).expect("bounded by construction"),
        );
        assert!(converted.is_some());
    }

    #[test]
    fn zero_attempts_is_refused_because_it_is_an_inconsistent_state() {
        // max_attempts = 0 would still be claimable (attempts increment at
        // claim), so a worker does hours of work on a budget already spent.
        assert_eq!(MaxAttempts::new(0), Err(InvalidQuantity::Zero));
        assert_eq!(MaxAttempts::new(1).map(MaxAttempts::get), Ok(1));
        assert_eq!(MaxAttempts::new(u32::MAX).map(MaxAttempts::get), Ok(u32::MAX));
    }

    #[test]
    fn refusals_render_something_an_operator_can_act_on() {
        assert!(InvalidQuantity::Zero.to_string().contains("at least 1"));
        let rendered = InvalidQuantity::TooManySeconds(MAX_SECONDS + 1).to_string();
        assert!(rendered.contains(&(MAX_SECONDS + 1).to_string()), "{rendered}");
        assert!(rendered.contains(&MAX_SECONDS.to_string()), "{rendered}");
    }
}
```

Add `pub mod quantity;` to `oxo-tasks/src/lib.rs` plus re-exports:

```rust
pub use quantity::{BackoffSeconds, InvalidQuantity, MaxAttempts, TimeoutSeconds, MAX_SECONDS};
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p oxo-tasks quantity`
Expected: compile failure — `BackoffSeconds` etc. not found. Capture the output.

- [ ] **Step 3: Implement the quantities**

Above the test module in `oxo-tasks/src/quantity.rs`:

```rust
//! Checked whole-second and attempt quantities for the request surface.
//!
//! These exist to remove a class of defect: `std::time::Duration` on the
//! port forced every adapter to convert with
//! `chrono::Duration::from_std(…).unwrap_or(zero())`, whose failure case
//! inverted the caller's intent — an enormous timeout became "reclaim
//! everything now". A value that can be constructed here is representable
//! by every adapter, so the conversion sites are infallible and the
//! refusal lives in exactly one place.

use std::num::NonZeroU32;

use thiserror::Error;

/// Upper bound on any whole-second quantity the port accepts.
///
/// `chrono::Duration` counts milliseconds in an `i64`, so any seconds
/// value at or below this converts infallibly in every adapter. The bound
/// is representability, not policy — it is roughly 292 million years.
pub const MAX_SECONDS: u64 = (i64::MAX / 1_000) as u64;

/// Why a quantity was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InvalidQuantity {
    #[error("must be at least 1")]
    Zero,
    #[error("{0} seconds exceeds what every adapter can represent ({MAX_SECONDS})")]
    TooManySeconds(u64),
}

/// Delay before a failed or reaped task becomes claimable again.
///
/// Zero is legal and means "retry immediately": `oxo-spec` defaults
/// `backoff_seconds` to 0, so refusing it would make the default
/// specification unsubmittable. This deliberately narrows the settled
/// answer in the job-server design, which said "refuses zero" without
/// accounting for that default; the design document records the ruling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BackoffSeconds(u64);

impl BackoffSeconds {
    pub const ZERO: Self = Self(0);

    pub fn new(seconds: u64) -> Result<Self, InvalidQuantity> {
        if seconds > MAX_SECONDS {
            return Err(InvalidQuantity::TooManySeconds(seconds));
        }
        Ok(Self(seconds))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// An expiry bound for the reaper: heartbeat timeout or maximum task
/// duration. Zero is refused — a zero bound reclaims every in-flight task
/// on the next tick, which is never what an operator meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeoutSeconds(u64);

impl TimeoutSeconds {
    pub fn new(seconds: u64) -> Result<Self, InvalidQuantity> {
        if seconds == 0 {
            return Err(InvalidQuantity::Zero);
        }
        if seconds > MAX_SECONDS {
            return Err(InvalidQuantity::TooManySeconds(seconds));
        }
        Ok(Self(seconds))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// Total attempts allowed per task, including the first. At least 1:
/// zero is an inconsistent state — `claim` still hands the task out, so a
/// worker does hours of production on a budget that was already spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaxAttempts(NonZeroU32);

impl MaxAttempts {
    pub fn new(attempts: u32) -> Result<Self, InvalidQuantity> {
        NonZeroU32::new(attempts)
            .map(Self)
            .ok_or(InvalidQuantity::Zero)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxo-tasks quantity`
Expected: PASS, every test in the module.

- [ ] **Step 5: `make verify`, then commit**

```bash
git add oxo-tasks/src/quantity.rs oxo-tasks/src/lib.rs
git commit -m "feat(tasks): add checked second and attempt quantities to the port

BackoffSeconds (zero legal — oxo-spec defaults it), TimeoutSeconds
(zero refused — a zero reap bound is a reclaim storm) and MaxAttempts
(zero refused — claimable but never retryable is inconsistent), all
bounded so every adapter's chrono arithmetic is infallible. Nothing
consumes them yet; the next change threads them through the request
surface and removes the unwrap_or(zero) inversion they exist to kill.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Thread the quantities through the port and the in-memory adapter

**Files:**
- Modify: `oxo-tasks/src/request.rs`
- Modify: `oxo-tasks/src/store.rs`
- Modify: `oxo-tasks/src/memory.rs`
- Modify: `oxo-tasks/src/conformance.rs`

**Interfaces:**
- Produces: `CreateJob { region_code: String, revision: u32, max_attempts: MaxAttempts, backoff: BackoffSeconds, tasks: Vec<TaskSpec> }`; `ReapRequest { heartbeat_timeout: TimeoutSeconds, max_task_duration: TimeoutSeconds }`. Everything else on the port is unchanged.
- Consumes: Task 1's types.

**This Task ends with `make verify` green and `make verify-db` red** — `oxo-tasks-postgres` does not compile against the new signatures until Task 3. That is expected and must be said plainly in the commit body and the report; it is why Tasks 2 and 3 are adjacent.

- [ ] **Step 1: Change the request types (the "failing test" is the build)**

In `oxo-tasks/src/request.rs`: remove `use std::time::Duration;`, add `use crate::quantity::{BackoffSeconds, MaxAttempts, TimeoutSeconds};`, and change the two structs:

```rust
pub struct CreateJob {
    pub region_code: String,
    pub revision: u32,
    pub max_attempts: MaxAttempts,
    pub backoff: BackoffSeconds,
    pub tasks: Vec<TaskSpec>,
}
```

```rust
pub struct ReapRequest {
    pub heartbeat_timeout: TimeoutSeconds,
    pub max_task_duration: TimeoutSeconds,
}
```

(`ReapRequest` keeps its doc comment about server configuration; `CreateJob` keeps its. Update the module's own test to construct the new types — `MaxAttempts::new(3).expect("non-zero")`, `BackoffSeconds::new(60).expect("in range")`.)

- [ ] **Step 2: Run the build to see every consumer fail**

Run: `cargo test -p oxo-tasks --all-features 2>&1 | head -50`
Expected: compile errors in `memory.rs` and `conformance.rs` at every site that constructed or read the old types. This enumerates the thread-through sites; do not hunt them with grep.

- [ ] **Step 3: Thread through the in-memory adapter**

In `oxo-tasks/src/memory.rs`:
- The internal `Job` struct's `backoff: Duration` becomes `backoff: BackoffSeconds`; `max_attempts: u32` stays `u32`, assigned from `request.max_attempts.get()` at creation.
- Every conversion site that read `chrono::Duration::from_std(request.backoff).unwrap_or_else(|_| chrono::Duration::zero())` (and the equivalents for the two reap bounds) becomes the infallible form:

```rust
fn chrono_seconds(seconds: u64) -> chrono::Duration {
    chrono::Duration::seconds(i64::try_from(seconds).expect("bounded by MAX_SECONDS at construction"))
}
```

one private helper, used as `chrono_seconds(job.backoff.get())`, `chrono_seconds(request.heartbeat_timeout.get())`, `chrono_seconds(request.max_task_duration.get())`. The `expect` is honest: construction bounds the value, and this helper is unreachable with anything larger.

- [ ] **Step 4: Thread through the conformance fixtures**

In `oxo-tasks/src/conformance.rs`, the fixtures and every case that builds a `CreateJob` or `ReapRequest` change mechanically:

```rust
max_attempts: MaxAttempts::new(3).expect("non-zero"),
backoff: BackoffSeconds::new(60).expect("in range"),
```

```rust
ReapRequest {
    heartbeat_timeout: TimeoutSeconds::new(60).expect("non-zero"),
    max_task_duration: TimeoutSeconds::new(3_600).expect("non-zero"),
}
```

Preserve each case's existing numeric values exactly — this Task changes types, never behaviour. The compiler's error list from Step 2 is the complete worklist.

- [ ] **Step 5: Document the reclaim contract honestly**

In `oxo-tasks/src/store.rs`, amend the doc comments on `heartbeat`, `complete` and `fail`. For `heartbeat` replace the sentence claiming `LeaseLost` is the reclaim answer with:

```rust
    /// Assert that a lease is still held.
    ///
    /// Two errors both mean "you have lost this task; stop working":
    /// [`TaskStoreError::NotClaimed`] when the task was reaped and is
    /// pending again, and [`TaskStoreError::LeaseLost`] once another
    /// worker has re-claimed it. Which one a caller sees is a matter of
    /// timing, and callers must treat them identically.
```

Add the same two-variant sentence (one line: `/// A reclaimed task answers [`TaskStoreError::NotClaimed`] until re-claimed, then [`TaskStoreError::LeaseLost`]; both mean stop.`) to `complete` and `fail`.

- [ ] **Step 6: Run the library tests**

Run: `cargo test -p oxo-tasks --all-features`
Expected: PASS — every lib test and every conformance case against the in-memory adapter, unchanged in behaviour.

- [ ] **Step 7: `make verify` (NOT `verify-db`), then commit**

`make verify` must be green. `make verify-db` is expected red (the PostgreSQL adapter no longer compiles) — do not run it as a gate here.

```bash
git add oxo-tasks/src/request.rs oxo-tasks/src/store.rs oxo-tasks/src/memory.rs oxo-tasks/src/conformance.rs
git commit -m "feat(tasks): put checked quantities on the request surface

CreateJob carries MaxAttempts and BackoffSeconds; ReapRequest carries
two TimeoutSeconds. Every from_std(..).unwrap_or(zero) conversion in
the in-memory adapter is replaced by an infallible one — the inversion
where an enormous timeout silently became zero can no longer be
written. The reclaim contract on heartbeat/complete/fail now names
both NotClaimed and LeaseLost as the two spellings of \"stop working\".

oxo-tasks-postgres does not compile against the new signatures yet;
the next commit threads it through. make verify is green; make
verify-db is expectedly red until then.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Thread the quantities through the PostgreSQL adapter

**Files:**
- Modify: `oxo-tasks-postgres/src/lib.rs`

**Interfaces:**
- Consumes: the Task 2 signatures. No new surface.

- [ ] **Step 1: Let the compiler enumerate the sites**

Run: `cargo check -p oxo-tasks-postgres 2>&1 | head -40`
Expected: errors at every `from_std` conversion and every `.bind` of the old values. This is the worklist.

- [ ] **Step 2: Convert each site infallibly**

The adapter binds whole seconds into `bigint` columns and converts to `chrono::Duration` for timestamp arithmetic. Replace each `chrono::Duration::from_std(…).unwrap_or_else(…)` with the same shape as the in-memory helper:

```rust
fn chrono_seconds(seconds: u64) -> chrono::Duration {
    chrono::Duration::seconds(i64::try_from(seconds).expect("bounded by MAX_SECONDS at construction"))
}
```

and each bind of backoff/max_attempts with:

```rust
.bind(i64::try_from(request.backoff.get()).expect("bounded by MAX_SECONDS at construction"))
.bind(i64::from(request.max_attempts.get()))
```

The schema is untouched: `backoff_secs`/`max_attempts` are already `bigint` with non-negative CHECKs, and every value constructible in the new types satisfies them.

- [ ] **Step 3: Run the conformance suite against PostgreSQL**

Run: `make verify-db`
Expected: PASS — the same cases as before Task 2, plus the migration-idempotency test, all green. This closes the window opened by Task 2.

- [ ] **Step 4: `make verify`, then commit**

```bash
git add oxo-tasks-postgres/src/lib.rs
git commit -m "feat(tasks-pg): consume the checked quantities infallibly

Same change as the in-memory adapter: every from_std(..).unwrap_or
conversion becomes an expect over a bound the constructor enforced, so
the two adapters can no longer disagree about an unrepresentable value
— PostgreSQL used to refuse what the in-memory adapter silently zeroed.
make verify-db is green again.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: The conformance cases the final review named

**Files:**
- Modify: `oxo-tasks/src/conformance.rs`

**Interfaces:**
- Consumes: the existing fixtures (`one_task_job`, `two_task_job`, `two_tile_job`), `TestClock`, the `conformance_case!`/`conformance_suite!` macros and the `all_cases_are_registered` parity test, which enforces registration of every case added here.
- Produces: five cases every adapter is held to, named in Step 1.

Each case asserts behaviour that **already ships**, so the RED step is the bite check from the Global Constraints: run once with a wrong expectation, show the failure, restore the truth. Do them one at a time.

- [ ] **Step 1: Add the five cases**

Follow the existing case style exactly (async fn over a `Fixture`-provided store, `TestClock` injected, `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]` via the macro). The cases, with their load-bearing assertions:

**`completing_a_task_twice_reports_not_claimed`** — the likeliest duplicate call in production: a worker completes, the response is lost, it retries.

```rust
let claimed = store.claim(any_task("w1")).await.expect("claim").expect("a task");
let lease = lease_of(&claimed);
store.complete(lease).await.expect("first complete");
let error = store.complete(lease).await.expect_err("second complete must be refused");
assert!(
    matches!(error, TaskStoreError::NotClaimed { task_id } if task_id == claimed.task_id),
    "got {error:?}"
);
```

**`resuming_a_job_with_a_different_policy_is_a_conflict`** — the policy arm of `JobConflict`; only the task-set arm was covered. Create `two_task_job()`, then re-create with the identical task set but `backoff: BackoffSeconds::new(61).expect("in range")`, and separately with `max_attempts: MaxAttempts::new(4).expect("non-zero")`. Both must be `TaskStoreError::JobConflict` naming the region and revision.

**`two_revisions_of_one_region_are_separate_jobs`** — create `one_task_job()`, then the same spec with `revision: 2`. Both `created: true`, distinct `job_id`s, and completing revision 1's task leaves revision 2 `InProgress` while revision 1 reports `Complete`.

**`a_task_set_matches_regardless_of_order`** — create `two_task_job()`, then re-create with `tasks` reversed (`.into_iter().rev().collect()`). Must be `created: false` with the same `job_id`: set comparison, not sequence comparison.

**`claims_serve_the_oldest_claimable_task_first`** — create `one_task_job()` at the fixture clock's start, `clock.advance(Duration::from_secs(10))`, create a second job for a different region (`region_code: "EU".to_string()` over `one_task_job()`'s shape, tile `tile(48, 2)`), then claim twice with no type filter. The first claim must return the first job's task, the second the second's. This is the honest form of "first-in-first-out": ordering is by when a task became claimable (`claimable_at`, with an identity tie-break inside a single instant) — tasks created together share one instant, so the case spans two instants deliberately.

- [ ] **Step 2: Bite-check each case against the in-memory adapter**

For each case in turn: flip one expected value (`created: false` → `true`, `NotClaimed` → `LeaseLost`, swap the two claim expectations), run `cargo test -p oxo-tasks --features conformance <case_name>`, capture the failure, restore the truth, run again, capture the pass. Five failures and five passes in the report.

- [ ] **Step 3: Register the cases and let the parity test prove it**

Add each name to the `conformance_suite!` registration block. Run `cargo test -p oxo-tasks --features conformance all_cases_are_registered` — it fails if any new case is defined but unregistered. Then run the whole suite: `cargo test -p oxo-tasks --all-features`.

- [ ] **Step 4: Run the suite against PostgreSQL**

Run: `make verify-db`
Expected: PASS, including all five new cases. If `claims_serve_the_oldest_claimable_task_first` fails here and nowhere else, the adapters genuinely disagree about ordering — stop and report rather than loosening the assertion.

- [ ] **Step 5: `make verify`, then commit**

```bash
git add oxo-tasks/src/conformance.rs
git commit -m "test(tasks): cover the gaps the final review named

completing_a_task_twice_reports_not_claimed,
resuming_a_job_with_a_different_policy_is_a_conflict,
two_revisions_of_one_region_are_separate_jobs,
a_task_set_matches_regardless_of_order and
claims_serve_the_oldest_claimable_task_first — behaviour that already
shipped, now argued for in both adapters. Each case was observed
failing against a deliberately wrong expectation before being trusted.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: `find_job` joins the port

**Files:**
- Modify: `oxo-tasks/src/request.rs`
- Modify: `oxo-tasks/src/store.rs`
- Modify: `oxo-tasks/src/memory.rs`
- Modify: `oxo-tasks/src/conformance.rs`
- Modify: `oxo-tasks-postgres/src/lib.rs`

**Interfaces:**
- Produces: `FindJob { region_code: String, revision: u32 }` in `request.rs`; on the trait:

```rust
    /// Recover a job's identity from the one the operator knows.
    ///
    /// `Ok(None)` means no such job, which is not an error. This exists so
    /// a restarted control plane can find a running job without re-running
    /// the planner just to read `created: false` back from `create_job`.
    async fn find_job(&self, request: FindJob) -> Result<Option<JobId>, TaskStoreError>;
```

- [ ] **Step 1: Write the failing conformance case**

In `conformance.rs`:

```rust
pub async fn find_job_recovers_a_created_jobs_identity<F: Fixture>() {
    // ... fixture boilerplate per existing cases ...
    let created = store.create_job(one_task_job()).await.expect("create");
    let found = store
        .find_job(FindJob { region_code: "NA".to_string(), revision: 1 })
        .await
        .expect("find");
    assert_eq!(found, Some(created.job_id));
    let absent_revision = store
        .find_job(FindJob { region_code: "NA".to_string(), revision: 2 })
        .await
        .expect("find");
    assert_eq!(absent_revision, None);
    let absent_region = store
        .find_job(FindJob { region_code: "EU".to_string(), revision: 1 })
        .await
        .expect("find");
    assert_eq!(absent_region, None);
}
```

(Adjust the fixture's actual region/revision values to `one_task_job()`'s real ones.) Register it in `conformance_suite!`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p oxo-tasks --all-features find_job`
Expected: compile failure — no `find_job` on the trait, no `FindJob`.

- [ ] **Step 3: Implement — request type, trait method, in-memory**

`request.rs`:

```rust
/// Recover a job's identity from `(region_code, revision)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindJob {
    pub region_code: String,
    pub revision: u32,
}
```

Trait method as in **Interfaces**. In-memory: the adapter already keys jobs by identity or holds identity on the job record — return the matching `JobId` by scanning or by its existing index, whichever the current structure makes natural, without restructuring state.

- [ ] **Step 4: In-memory green**

Run: `cargo test -p oxo-tasks --all-features`
Expected: PASS including the new case; parity test still green.

- [ ] **Step 5: PostgreSQL adapter**

```rust
    async fn find_job(&self, request: FindJob) -> Result<Option<JobId>, TaskStoreError> {
        let row: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT id FROM jobs WHERE region_code = $1 AND revision = $2")
                .bind(&request.region_code)
                .bind(i64::from(request.revision))
                .fetch_optional(&self.pool)
                .await
                .map_err(adapter_error)?;
        Ok(row.map(|(id,)| JobId::from_uuid(id)))
    }
```

(Match the file's existing error-mapping helper name and bind style — read the neighbouring methods first; the `revision` bind must match the column's existing type treatment exactly as `create_job` binds it.)

- [ ] **Step 6: `make verify-db`**

Expected: PASS including `find_job_recovers_a_created_jobs_identity` against PostgreSQL.

- [ ] **Step 7: `make verify`, then commit**

```bash
git add oxo-tasks/src/request.rs oxo-tasks/src/store.rs oxo-tasks/src/memory.rs oxo-tasks/src/conformance.rs oxo-tasks-postgres/src/lib.rs
git commit -m "feat(tasks): recover a job's identity with find_job

FindJob { region_code, revision } -> Option<JobId> on the port, held
to conformance in both adapters. Without it, a restarted control plane
could only recover a handle by re-running the planner and reading
created: false back from create_job.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: The `oxo-control` crate and the planner

**Files:**
- Modify: `Cargo.toml` (workspace members, `rust-version = "1.75"`)
- Create: `oxo-control/Cargo.toml`
- Create: `oxo-control/src/lib.rs`
- Create: `oxo-control/src/planner.rs`

**Interfaces:**
- Produces: `oxo_control::planner::plan(&RegionSpec) -> Result<CreateJob, PlanError>`; `PlanError` (`MaxAttempts(InvalidQuantity)`, `Backoff(InvalidQuantity)`).
- Consumes: `oxo_spec::RegionSpec` (fields `tiles: BTreeSet<TileId>`, `metadata.region_code`, `metadata.revision`, `parameters.include_overlays`, `failure_policy.max_attempts`, `failure_policy.backoff_seconds`); `oxo_tasks::{CreateJob, TaskSpec, TaskType, MaxAttempts, BackoffSeconds}`.

- [ ] **Step 1: Workspace membership and the floor**

Root `Cargo.toml`: add `"oxo-control"` to `members`; change `rust-version = "1.74"` to `rust-version = "1.75"` (required by axum 0.8, arriving in Task 7; raised here so the floor change travels with the crate that needs it).

`oxo-control/Cargo.toml`:

```toml
[package]
name = "oxo-control"
description = "OXO control plane: planner, claim API and reaper over the task-store port"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
oxo-spec = { path = "../oxo-spec" }
oxo-tasks = { path = "../oxo-tasks" }
thiserror = { workspace = true }
```

`oxo-control/src/lib.rs`:

```rust
//! OXO control plane library: the planner that atomizes a region
//! specification into tasks, the HTTP API workers claim and report
//! through, and the reaper loop that drives lease expiry.
//!
//! This crate depends on the task-store **port**, never on an adapter:
//! no sqlx, no database driver. The composition root (`oxo-controld`)
//! injects the adapter behind `Arc<dyn TaskStore>`.

#![forbid(unsafe_code)]

pub mod planner;
```

- [ ] **Step 2: Write the failing planner tests**

In `oxo-control/src/planner.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    use oxo_spec::{
        FailurePolicy, Metadata, ProductionParameters, RegionSpec, TargetLocation, TileId,
    };
    use oxo_tasks::TaskType;

    use super::*;

    fn spec(tiles: &[(i8, i16)], include_overlays: bool) -> RegionSpec {
        RegionSpec {
            tiles: tiles
                .iter()
                .map(|&(lat, lon)| TileId::new(lat, lon).expect("in range"))
                .collect::<BTreeSet<_>>(),
            metadata: Metadata {
                name: "North America".to_string(),
                region_code: "NA".to_string(),
                revision: 1,
            },
            parameters: ProductionParameters {
                provider: "BI".to_string(),
                zoom: 16,
                include_overlays,
                raw: BTreeMap::new(),
            },
            target: TargetLocation {
                root: "/srv/oxo/artifacts/NA".to_string(),
            },
            failure_policy: FailurePolicy {
                max_attempts: 3,
                backoff_seconds: 60,
                alert_destinations: Vec::new(),
            },
        }
    }

    #[test]
    fn without_overlays_each_tile_becomes_one_ortho_task() {
        let job = plan(&spec(&[(50, -2), (51, -2)], false)).expect("plan");
        assert_eq!(job.tasks.len(), 2);
        assert!(job.tasks.iter().all(|t| t.task_type == TaskType::Ortho));
    }

    #[test]
    fn with_overlays_each_tile_becomes_an_ortho_and_an_overlay_task() {
        let job = plan(&spec(&[(50, -2), (51, -2)], true)).expect("plan");
        let types: Vec<_> = job.tasks.iter().map(|t| t.task_type).collect();
        assert_eq!(
            types,
            vec![TaskType::Ortho, TaskType::Overlay, TaskType::Ortho, TaskType::Overlay]
        );
        assert_eq!(job.tasks[0].tile, job.tasks[1].tile);
        assert_ne!(job.tasks[0].tile, job.tasks[2].tile);
    }

    #[test]
    fn identity_and_policy_are_copied_from_the_specification() {
        let job = plan(&spec(&[(50, -2)], false)).expect("plan");
        assert_eq!(job.region_code, "NA");
        assert_eq!(job.revision, 1);
        assert_eq!(job.max_attempts.get(), 3);
        assert_eq!(job.backoff.get(), 60);
    }

    #[test]
    fn planning_is_deterministic_for_an_unchanged_specification() {
        // create_job's idempotency compares task sets; a planner that
        // reordered between runs would still resume (comparison is
        // order-insensitive) but reproducibility is the promise here.
        let first = plan(&spec(&[(51, -2), (50, -2), (50, -3)], true)).expect("plan");
        let second = plan(&spec(&[(50, -3), (51, -2), (50, -2)], true)).expect("plan");
        assert_eq!(first, second);
    }

    #[test]
    fn an_unvalidated_zero_max_attempts_is_refused_not_unwrapped() {
        // oxo-spec validation already refuses this, but plan's input type
        // cannot prove its argument was validated, so the error must be a
        // value, never a panic.
        let mut bad = spec(&[(50, -2)], false);
        bad.failure_policy.max_attempts = 0;
        let error = plan(&bad).expect_err("refuse");
        assert!(matches!(error, PlanError::MaxAttempts(InvalidQuantity::Zero)));
        assert!(error.to_string().contains("max_attempts"), "{error}");
    }

    #[test]
    fn an_unrepresentable_backoff_is_refused_not_unwrapped() {
        let mut bad = spec(&[(50, -2)], false);
        bad.failure_policy.backoff_seconds = u64::MAX;
        let error = plan(&bad).expect_err("refuse");
        assert!(matches!(error, PlanError::Backoff(InvalidQuantity::TooManySeconds(_))));
    }
}
```

(If `Metadata`/`TargetLocation` field names differ from the above, read `oxo-spec/src/metadata.rs` and `target.rs` and use the real ones — the test must construct real specs, and all fields are public.)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p oxo-control`
Expected: compile failure — `plan` and `PlanError` do not exist.

- [ ] **Step 4: Implement the planner**

Above the tests:

```rust
//! Specification → task set. One ortho task per tile; one overlay task
//! per tile additionally when the specification asks for overlays. Up to
//! 2N tasks from N tiles. `include_overlays = false` yields N ortho tasks
//! and is a first-class choice, not a degraded mode.

use oxo_spec::RegionSpec;
use oxo_tasks::{BackoffSeconds, CreateJob, InvalidQuantity, MaxAttempts, TaskSpec, TaskType};
use thiserror::Error;

/// Why a specification could not be planned.
///
/// Both cases are unreachable from a specification that passed
/// `oxo-spec` validation — it already refuses `max_attempts = 0` — but
/// `plan`'s input type cannot prove that, so the refusal is propagated
/// as a value, never unwrapped.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("failure policy max_attempts: {0}")]
    MaxAttempts(InvalidQuantity),
    #[error("failure policy backoff_seconds: {0}")]
    Backoff(InvalidQuantity),
}

/// Atomize a validated region specification into a job registration.
///
/// Deterministic: tiles in `BTreeSet` order, each tile's ortho task
/// before its overlay task, so re-planning an unchanged specification
/// reproduces the task set element for element.
pub fn plan(spec: &RegionSpec) -> Result<CreateJob, PlanError> {
    let max_attempts =
        MaxAttempts::new(spec.failure_policy.max_attempts).map_err(PlanError::MaxAttempts)?;
    let backoff =
        BackoffSeconds::new(spec.failure_policy.backoff_seconds).map_err(PlanError::Backoff)?;

    let per_tile = if spec.parameters.include_overlays { 2 } else { 1 };
    let mut tasks = Vec::with_capacity(spec.tiles.len() * per_tile);
    for &tile in &spec.tiles {
        tasks.push(TaskSpec { tile, task_type: TaskType::Ortho });
        if spec.parameters.include_overlays {
            tasks.push(TaskSpec { tile, task_type: TaskType::Overlay });
        }
    }

    Ok(CreateJob {
        region_code: spec.metadata.region_code.clone(),
        revision: spec.metadata.revision,
        max_attempts,
        backoff,
        tasks,
    })
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p oxo-control`
Expected: PASS.

- [ ] **Step 6: `make verify`, then commit**

```bash
git add Cargo.toml oxo-control/Cargo.toml oxo-control/src/lib.rs oxo-control/src/planner.rs
git commit -m "feat(control): the planner — specification to task set

New oxo-control crate (port consumers only, never an adapter). plan()
emits one ortho task per tile plus one overlay task per tile when the
specification asks, deterministically ordered, policy copied through
the checked quantities. Raises the workspace rust floor to 1.75 for
axum 0.8, which the API tasks consume next.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Wire types and the one error mapping

**Files:**
- Modify: `Cargo.toml` (workspace dependencies)
- Modify: `oxo-control/Cargo.toml`
- Create: `oxo-control/src/api/mod.rs`
- Create: `oxo-control/src/api/wire.rs`
- Create: `oxo-control/src/api/error.rs`
- Modify: `oxo-control/src/lib.rs`

**Interfaces:**
- Produces (wire, all in `api::wire`): `JobCreatedBody { job_id: Uuid, created: bool, total_tasks: u32 }` (Serialize); `JobStatusBody` (Serialize, tagged by `state`: `complete` / `failed { abandoned }` / `in_progress { pending, claimed, succeeded, abandoned }`); `ThroughputBody` (Serialize, the five counts); `FoundJobBody { job_id: Uuid }`; `ClaimBody { worker: String, task_types: Option<Vec<String>> }` (Deserialize); `ClaimedTaskBody { task_id, job_id, lease_token: Uuid, tile: String, task_type: String, attempt: u32 }` (Serialize); `LeaseBody { lease_token: Uuid }` (Deserialize); `FailBody { lease_token: Uuid, reason: String }` (Deserialize); `FailOutcomeBody` (Serialize, tagged by `outcome`: `requeued { claimable_at: DateTime<Utc>, attempts_remaining }` / `abandoned`). Conversions from the port types (`From<JobCreated>`, `From<JobStatus>`, `From<Throughput>`, `From<ClaimedTask>`, `From<FailOutcome>`).
- Produces (`api::error`): `ApiError` with `impl IntoResponse` — the single mapping table; `ErrorBody { error: &'static str, message: String }`.
- Consumes: Task 6's crate.

- [ ] **Step 1: Dependencies**

Workspace `Cargo.toml`, `[workspace.dependencies]` additions:

```toml
axum = "0.8"
serde_json = "1"
tower = { version = "0.5", features = ["util"] }
http-body-util = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tower-http = { version = "0.6", features = ["trace"] }
```

and amend two existing lines by adding a feature each: `chrono = { …, features = ["clock", "std", "serde"] }`, `uuid = { version = "1", features = ["v4", "serde"] }`.

`oxo-control/Cargo.toml` gains:

```toml
axum = { workspace = true }
chrono = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
uuid = { workspace = true }

[dev-dependencies]
http-body-util = { workspace = true }
tower = { workspace = true }
```

`lib.rs` gains `pub mod api;`; `api/mod.rs` starts as:

```rust
//! The HTTP surface. Wire types are owned here, serialized with serde;
//! the port types in oxo-tasks stay serde-free, so the wire contract can
//! change without touching the port.

pub mod error;
pub mod wire;
```

- [ ] **Step 2: Write the failing tests**

In `api/wire.rs`, a test module asserting the exact JSON shapes (these are the wire contract — a worker in another language parses them, so the strings in these tests are the specification):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_status_serializes_tagged_by_state() {
        let complete = serde_json::to_value(JobStatusBody::from(JobStatus::Complete)).unwrap();
        assert_eq!(complete, serde_json::json!({"state": "complete"}));

        let failed = serde_json::to_value(JobStatusBody::from(JobStatus::Failed { abandoned: 2 })).unwrap();
        assert_eq!(failed, serde_json::json!({"state": "failed", "abandoned": 2}));

        let in_progress = serde_json::to_value(JobStatusBody::from(JobStatus::InProgress {
            pending: 1, claimed: 2, succeeded: 3, abandoned: 0,
        })).unwrap();
        assert_eq!(
            in_progress,
            serde_json::json!({"state": "in_progress", "pending": 1, "claimed": 2, "succeeded": 3, "abandoned": 0})
        );
    }

    #[test]
    fn a_claimed_task_serializes_its_tile_and_type_canonically() {
        let claimed = ClaimedTask {
            task_id: TaskId::generate(),
            job_id: JobId::generate(),
            lease: LeaseToken::generate(),
            tile: TileId::new(50, -2).expect("in range"),
            task_type: TaskType::Overlay,
            attempt: 1,
        };
        let body = serde_json::to_value(ClaimedTaskBody::from(claimed)).unwrap();
        assert_eq!(body["tile"], "+50-002");
        assert_eq!(body["task_type"], "overlay");
        assert_eq!(body["attempt"], 1);
    }

    #[test]
    fn a_fail_outcome_serializes_tagged_by_outcome() {
        let abandoned = serde_json::to_value(FailOutcomeBody::from(FailOutcome::Abandoned)).unwrap();
        assert_eq!(abandoned, serde_json::json!({"outcome": "abandoned"}));
        // Requeued carries an RFC 3339 claimable_at and the remaining budget.
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-02T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let requeued = serde_json::to_value(FailOutcomeBody::from(FailOutcome::Requeued {
            claimable_at: at, attempts_remaining: 2,
        })).unwrap();
        assert_eq!(requeued["outcome"], "requeued");
        assert_eq!(requeued["attempts_remaining"], 2);
        assert!(requeued["claimable_at"].as_str().unwrap().starts_with("2026-10-02T12:00:00"));
    }

    #[test]
    fn claim_bodies_distinguish_absent_from_empty_task_types() {
        let any: ClaimBody = serde_json::from_str(r#"{"worker": "w1"}"#).unwrap();
        assert_eq!(any.task_types, None);
        let none: ClaimBody = serde_json::from_str(r#"{"worker": "w1", "task_types": []}"#).unwrap();
        assert_eq!(none.task_types, Some(vec![]));
    }
}
```

In `api/error.rs`, the mapping-table test — every variant, status and code asserted:

```rust
#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::*;

    fn status_and_code(error: ApiError) -> (StatusCode, String) {
        let response = error.into_response();
        let status = response.status();
        let bytes = // collect body with http_body_util::BodyExt::collect (block_on via tokio::test)
        // parse ErrorBody JSON, return (status, body.error)
    }

    #[tokio::test]
    async fn every_store_error_maps_per_the_design_table() {
        let task_id = TaskId::generate();
        let job_id = JobId::generate();
        let tile = TileId::new(50, -2).expect("in range");
        let cases = vec![
            (TaskStoreError::LeaseLost { task_id }, StatusCode::CONFLICT, "lease_lost"),
            (TaskStoreError::NotClaimed { task_id }, StatusCode::CONFLICT, "not_claimed"),
            (TaskStoreError::UnknownJob { job_id }, StatusCode::NOT_FOUND, "unknown_job"),
            (TaskStoreError::UnknownTask { task_id }, StatusCode::NOT_FOUND, "unknown_task"),
            (
                TaskStoreError::JobConflict { region_code: "NA".into(), revision: 1 },
                StatusCode::CONFLICT,
                "job_conflict",
            ),
            (
                TaskStoreError::DuplicateTask { tile, task_type: TaskType::Ortho },
                StatusCode::UNPROCESSABLE_ENTITY,
                "duplicate_task",
            ),
            (
                TaskStoreError::EmptyJob { region_code: "NA".into(), revision: 1 },
                StatusCode::UNPROCESSABLE_ENTITY,
                "empty_job",
            ),
            (
                TaskStoreError::Adapter("connection reset".into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "adapter",
            ),
        ];
        for (error, want_status, want_code) in cases { /* assert via status_and_code */ }
    }

    #[tokio::test]
    async fn api_level_refusals_map_to_their_own_codes() {
        // InvalidSpec -> 422 "invalid_spec", message carries the full report text
        // UnknownTaskType -> 422 "unknown_task_type"
        // NoSuchJob -> 404 "unknown_job"
    }
}
```

(The commented skeleton lines must become real code; the `status_and_code` helper collects the body with `response.into_body()` + `http_body_util::BodyExt::collect(…).await` and `serde_json::from_slice`.)

- [ ] **Step 3: Run to verify failure, then implement**

`wire.rs`: plain structs with `#[derive(Serialize)]`/`#[derive(Deserialize)]` and `From` impls; `JobStatusBody` and `FailOutcomeBody` are enums with `#[serde(tag = "state", rename_all = "snake_case")]` / `#[serde(tag = "outcome", rename_all = "snake_case")]`; `tile` serialized via `TileId`'s `Display`, `task_type` via `TaskType::as_str()`; ids via `as_uuid()`.

`error.rs`:

```rust
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use oxo_tasks::TaskStoreError;
use serde::Serialize;

/// The one place a fault becomes a status code. Handlers return
/// `Result<_, ApiError>` and never choose their own mapping.
#[derive(Debug)]
pub enum ApiError {
    Store(TaskStoreError),
    /// The submitted specification failed to parse or validate; the
    /// message is the full all-faults report, exactly what the CLI prints.
    InvalidSpec(String),
    UnknownTaskType(String),
    NoSuchJob { region_code: String, revision: u32 },
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: &'static str,
    pub message: String,
}

impl From<TaskStoreError> for ApiError {
    fn from(error: TaskStoreError) -> Self {
        Self::Store(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Store(error) => {
                let (status, code) = match &error {
                    TaskStoreError::LeaseLost { .. } => (StatusCode::CONFLICT, "lease_lost"),
                    TaskStoreError::NotClaimed { .. } => (StatusCode::CONFLICT, "not_claimed"),
                    TaskStoreError::UnknownJob { .. } => (StatusCode::NOT_FOUND, "unknown_job"),
                    TaskStoreError::UnknownTask { .. } => (StatusCode::NOT_FOUND, "unknown_task"),
                    TaskStoreError::JobConflict { .. } => (StatusCode::CONFLICT, "job_conflict"),
                    TaskStoreError::DuplicateTask { .. } => {
                        (StatusCode::UNPROCESSABLE_ENTITY, "duplicate_task")
                    }
                    TaskStoreError::EmptyJob { .. } => {
                        (StatusCode::UNPROCESSABLE_ENTITY, "empty_job")
                    }
                    TaskStoreError::Adapter(_) => (StatusCode::SERVICE_UNAVAILABLE, "adapter"),
                    // The port is #[non_exhaustive]; an unknown variant is
                    // an adapter-shaped surprise, not a client fault.
                    _ => (StatusCode::SERVICE_UNAVAILABLE, "adapter"),
                };
                (status, code, error.to_string())
            }
            Self::InvalidSpec(report) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "invalid_spec", report)
            }
            Self::UnknownTaskType(name) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "unknown_task_type",
                format!("unknown task type {name:?}; expected \"ortho\" or \"overlay\""),
            ),
            Self::NoSuchJob { region_code, revision } => (
                StatusCode::NOT_FOUND,
                "unknown_job",
                format!("no job for region {region_code} revision {revision}"),
            ),
        };
        (status, Json(ErrorBody { error: code, message })).into_response()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxo-control`
Expected: PASS.

- [ ] **Step 5: `make verify`, then commit**

```bash
git add Cargo.toml oxo-control/Cargo.toml oxo-control/src/lib.rs oxo-control/src/api
git commit -m "feat(control): wire types and the one error mapping

The JSON contract is owned here and pinned by tests that assert exact
shapes — a worker in another language parses these strings. One
IntoResponse carries the whole design mapping table, so a handler
cannot choose its own status for a store error. The port types stay
serde-free.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Operator endpoints

**Files:**
- Create: `oxo-control/src/api/jobs.rs`
- Modify: `oxo-control/src/api/mod.rs`

**Interfaces:**
- Produces: `api::router(store: Arc<dyn TaskStore>) -> axum::Router` serving `POST /api/v1/jobs`, `GET /api/v1/jobs` (query `region_code`, `revision`), `GET /api/v1/jobs/{job_id}`, `GET /api/v1/jobs/{job_id}/throughput`, `GET /healthz`. (Worker routes join the same router in Task 9.)
- Consumes: the planner (Task 6), the wire and error modules (Task 7).

- [ ] **Step 1: Router scaffolding**

`api/mod.rs` grows:

```rust
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use oxo_tasks::TaskStore;

mod jobs;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<dyn TaskStore>,
}

/// The whole HTTP surface over an injected store. The composition root
/// decides which adapter sits behind it; tests inject the in-memory one.
pub fn router(store: Arc<dyn TaskStore>) -> Router {
    Router::new()
        .route("/healthz", get(|| async {}))
        .route("/api/v1/jobs", post(jobs::submit).get(jobs::find))
        .route("/api/v1/jobs/{job_id}", get(jobs::status))
        .route("/api/v1/jobs/{job_id}/throughput", get(jobs::throughput))
        .with_state(AppState { store })
}
```

- [ ] **Step 2: Write the failing handler tests**

Tests live in `api/jobs.rs`'s test module, exercising the real router end to end with `tower::ServiceExt::oneshot` over the in-memory store. One helper builds the app:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use oxo_tasks::memory::InMemoryTaskStore; // match the real exported name
    use oxo_tasks::TestClock;                 // match the real exported path
    use tower::ServiceExt;

    use crate::api::router;

    const SPEC: &str = r#"
tiles = ["+50-002", "+51-002"]

[metadata]
name = "North America"
region_code = "NA"
revision = 1

[parameters]
provider = "BI"
zoom = 16
include_overlays = true

[target]
root = "/srv/oxo/artifacts/NA"

[failure_policy]
max_attempts = 3
backoff_seconds = 60
"#;

    // helper: app() -> Router wired to a fresh in-memory store + TestClock
    // helper: post_toml(app, "/api/v1/jobs", SPEC) -> (StatusCode, serde_json::Value)

    #[tokio::test]
    async fn submitting_a_specification_creates_the_planned_job() {
        // 201; body.created == true; body.total_tasks == 4 (two tiles,
        // overlays on); job_id parses as a UUID
    }

    #[tokio::test]
    async fn resubmitting_the_same_specification_resumes_not_duplicates() {
        // first POST 201; second POST 200 with created == false and the
        // same job_id
    }

    #[tokio::test]
    async fn a_faulty_specification_gets_the_whole_validation_report() {
        // POST the SPEC with `tiles = []` substituted: 422, error ==
        // "invalid_spec", message contains "tile set is empty"
    }

    #[tokio::test]
    async fn unparseable_toml_is_refused_as_invalid_spec() {
        // POST "this is not toml": 422, error == "invalid_spec",
        // message contains "could not parse specification"
    }

    #[tokio::test]
    async fn a_job_is_findable_by_the_identity_the_operator_knows() {
        // POST the spec, then GET /api/v1/jobs?region_code=NA&revision=1
        // -> 200 with the same job_id; revision=2 -> 404 "unknown_job"
    }

    #[tokio::test]
    async fn status_and_throughput_answer_for_a_real_job_and_404_otherwise() {
        // POST, then GET /api/v1/jobs/{id} -> 200 {"state":"in_progress",
        // "pending":4,...}; GET /api/v1/jobs/{id}/throughput -> 200 with
        // pending == 4 and claimable_now == 4; a random UUID -> 404
    }

    #[tokio::test]
    async fn healthz_answers_200() {}
}
```

Every `// helper` and `// body` comment above must become real code — they are the test specification, not optional notes. The TOML string in `SPEC` matches `oxo-spec`'s canonical format (see `oxo-spec/src/spec.rs` test constants for the shape; tiles are strings like `"+50-002"`).

- [ ] **Step 3: Run to verify failure, then implement the handlers**

`api/jobs.rs`:

```rust
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use oxo_spec::RegionSpec;
use oxo_tasks::{FindJob, JobId};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::api::wire::{FoundJobBody, JobCreatedBody, JobStatusBody, ThroughputBody};
use crate::api::AppState;
use crate::planner;

/// POST /api/v1/jobs — body is the canonical specification TOML.
pub(crate) async fn submit(
    State(state): State<AppState>,
    body: String,
) -> Result<(StatusCode, Json<JobCreatedBody>), ApiError> {
    let spec = RegionSpec::from_toml(&body).map_err(|e| ApiError::InvalidSpec(e.to_string()))?;
    let job = planner::plan(&spec).map_err(|e| ApiError::InvalidSpec(e.to_string()))?;
    let created = state.store.create_job(job).await?;
    let status = if created.created { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(JobCreatedBody::from(created))))
}

#[derive(Deserialize)]
pub(crate) struct FindQuery {
    region_code: String,
    revision: u32,
}

/// GET /api/v1/jobs?region_code=NA&revision=1
pub(crate) async fn find(
    State(state): State<AppState>,
    Query(query): Query<FindQuery>,
) -> Result<Json<FoundJobBody>, ApiError> {
    let found = state
        .store
        .find_job(FindJob { region_code: query.region_code.clone(), revision: query.revision })
        .await?;
    match found {
        Some(job_id) => Ok(Json(FoundJobBody { job_id: job_id.as_uuid() })),
        None => Err(ApiError::NoSuchJob { region_code: query.region_code, revision: query.revision }),
    }
}

/// GET /api/v1/jobs/{job_id}
pub(crate) async fn status(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<JobStatusBody>, ApiError> {
    let status = state.store.job_status(JobId::from_uuid(job_id)).await?;
    Ok(Json(JobStatusBody::from(status)))
}

/// GET /api/v1/jobs/{job_id}/throughput
pub(crate) async fn throughput(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<ThroughputBody>, ApiError> {
    let snapshot = state.store.throughput(JobId::from_uuid(job_id)).await?;
    Ok(Json(ThroughputBody::from(snapshot)))
}
```

(Match the in-memory store's real constructor and the `TestClock` path when writing the helpers; read `oxo-tasks/src/memory.rs`'s test setup for the canonical wiring.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxo-control`
Expected: PASS.

- [ ] **Step 5: `make verify`, then commit**

```bash
git add oxo-control/src/api
git commit -m "feat(control): operator endpoints — submit, find, status, throughput

POST /api/v1/jobs takes the canonical specification TOML, answers 201
on creation and 200 on resume, and returns the full all-faults
validation report on 422 — one round trip to fix everything. Status,
throughput and identity recovery are reads over the port. Tested end
to end through the real router with the in-memory store.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: Worker endpoints

**Files:**
- Create: `oxo-control/src/api/tasks.rs`
- Modify: `oxo-control/src/api/mod.rs`

**Interfaces:**
- Produces, on the same router: `POST /api/v1/claims`, `POST /api/v1/tasks/{task_id}/heartbeat`, `POST /api/v1/tasks/{task_id}/complete`, `POST /api/v1/tasks/{task_id}/fail`.

- [ ] **Step 1: Routes**

In `api/mod.rs`:

```rust
        .route("/api/v1/claims", post(tasks::claim))
        .route("/api/v1/tasks/{task_id}/heartbeat", post(tasks::heartbeat))
        .route("/api/v1/tasks/{task_id}/complete", post(tasks::complete))
        .route("/api/v1/tasks/{task_id}/fail", post(tasks::fail))
```

- [ ] **Step 2: Write the failing handler tests**

In `api/tasks.rs`'s test module, same harness as Task 8 (submit `SPEC` first to populate the store):

```rust
    #[tokio::test]
    async fn a_claim_hands_out_a_task_with_a_lease() {
        // POST /api/v1/claims {"worker": "w1"} -> 200; body has task_id,
        // job_id, lease_token (UUIDs), tile "+50-002" or "+51-002",
        // task_type "ortho" or "overlay", attempt 1
    }

    #[tokio::test]
    async fn an_empty_queue_claims_nothing_with_204() {
        // fresh app, no job: POST claims -> 204, empty body
    }

    #[tokio::test]
    async fn a_type_filter_restricts_what_a_worker_receives() {
        // {"worker":"w1","task_types":["overlay"]} -> only overlay tasks;
        // {"worker":"w1","task_types":[]} -> 204 (no capacity means no work);
        // {"worker":"w1","task_types":["mesh"]} -> 422 "unknown_task_type"
    }

    #[tokio::test]
    async fn the_full_report_cycle_heartbeat_complete() {
        // claim; heartbeat {"lease_token": ...} -> 204; complete -> 204;
        // complete again -> 409 "not_claimed" (the worker-protocol rule:
        // any 409 on a report means stop)
    }

    #[tokio::test]
    async fn a_failure_reports_its_outcome() {
        // claim; POST fail {"lease_token":..., "reason":"Crash!"} -> 200
        // {"outcome":"requeued", "claimable_at": ..., "attempts_remaining": 2}
        // (SPEC has max_attempts 3); claim+fail until
        // {"outcome":"abandoned"}
    }

    #[tokio::test]
    async fn a_stale_lease_is_409_with_lease_lost_or_not_claimed() {
        // claim with a TestClock-driven app; stop heartbeating; drive
        // reap_expired directly on the store (the reaper loop is Task 10);
        // then heartbeat with the old lease -> 409, body error is
        // "not_claimed" (pending again, nobody re-claimed); after another
        // worker claims it, heartbeat with the old lease -> 409 "lease_lost"
    }

    #[tokio::test]
    async fn reporting_on_an_unknown_task_is_404() {
        // random UUID path, any lease -> 404 "unknown_task"
    }
```

Every skeleton comment becomes real code.

- [ ] **Step 3: Implement the handlers**

```rust
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use oxo_tasks::{ClaimRequest, FailRequest, Lease, LeaseToken, TaskId, TaskType};
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::api::wire::{ClaimBody, ClaimedTaskBody, FailBody, FailOutcomeBody, LeaseBody};
use crate::api::AppState;

/// POST /api/v1/claims — 200 with a task, or 204 when nothing is
/// claimable, which is the normal idle state of a pull system.
pub(crate) async fn claim(
    State(state): State<AppState>,
    Json(body): Json<ClaimBody>,
) -> Result<Response, ApiError> {
    let task_types = match body.task_types {
        None => None,
        Some(names) => Some(
            names
                .into_iter()
                .map(|name| {
                    TaskType::from_str_exact(&name).ok_or(ApiError::UnknownTaskType(name))
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    let claimed = state
        .store
        .claim(ClaimRequest { worker: body.worker, task_types })
        .await?;
    Ok(match claimed {
        Some(task) => (StatusCode::OK, Json(ClaimedTaskBody::from(task))).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

fn lease(task_id: Uuid, token: Uuid) -> Lease {
    Lease { task_id: TaskId::from_uuid(task_id), token: LeaseToken::from_uuid(token) }
}

pub(crate) async fn heartbeat(
    State(state): State<AppState>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<LeaseBody>,
) -> Result<StatusCode, ApiError> {
    state.store.heartbeat(lease(task_id, body.lease_token)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn complete(
    State(state): State<AppState>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<LeaseBody>,
) -> Result<StatusCode, ApiError> {
    state.store.complete(lease(task_id, body.lease_token)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn fail(
    State(state): State<AppState>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<FailBody>,
) -> Result<Json<FailOutcomeBody>, ApiError> {
    let outcome = state
        .store
        .fail(FailRequest { lease: lease(task_id, body.lease_token), reason: body.reason })
        .await?;
    Ok(Json(FailOutcomeBody::from(outcome)))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxo-control`
Expected: PASS.

- [ ] **Step 5: `make verify`, then commit**

```bash
git add oxo-control/src/api
git commit -m "feat(control): worker endpoints — claim, heartbeat, complete, fail

The pull protocol over the port: 200 hands out a task and its lease,
204 means nothing claimable and is not an error, any 409 on a report
means the worker has lost the task and stops. Unknown task types are
refused at the wire, before the port sees them.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: The reaper loop

**Files:**
- Create: `oxo-control/src/reaper.rs`
- Modify: `oxo-control/src/lib.rs`

**Interfaces:**
- Produces:

```rust
/// Drive lease expiry forever: call `reap_expired` every `every`, log
/// passes that reclaimed anything, and keep going when a pass fails —
/// a transient database outage must not end lease enforcement.
pub async fn run(store: Arc<dyn TaskStore>, request: ReapRequest, every: std::time::Duration)
```

- Consumes: `ReapRequest` (Task 2's `TimeoutSeconds` fields), `tokio::time`, `tracing`.

- [ ] **Step 1: Write the failing tests**

`reaper.rs` test module, `start_paused` tokio time. Two tests:

```rust
    #[tokio::test(start_paused = true)]
    async fn the_reaper_reclaims_an_expired_lease_on_schedule() {
        // in-memory store + TestClock; create one_task_job-shaped job,
        // claim it, no heartbeats. Spawn run(store, request, 30s) with a
        // 60s heartbeat timeout. Advance TestClock by 61s AND
        // tokio::time by 90s (the loop runs on tokio time; expiry is
        // judged by the store's clock — both must move). Yield, then
        // assert the task is claimable again: a fresh claim succeeds and
        // its attempt is 2. Abort the spawned loop.
    }

    #[tokio::test(start_paused = true)]
    async fn an_adapter_error_does_not_end_the_loop() {
        // a FlakyStore test double wrapping the in-memory store: its
        // reap_expired fails with TaskStoreError::Adapter("down") the
        // first N calls, then delegates. Spawn the loop; advance past
        // several intervals; assert a later pass still reaped (same
        // observable as above). The double implements TaskStore by
        // delegating every other method.
    }
```

Both comments become real code. The `FlakyStore` double lives in the test module.

- [ ] **Step 2: Run to verify failure, then implement**

```rust
//! Lease expiry is a correctness obligation of the control plane, not
//! optional maintenance — a worker that dies without this loop holds its
//! task forever. The loop lives in the library so its behaviour is
//! testable; the composition root only spawns it.

use std::sync::Arc;

use oxo_tasks::{ReapRequest, TaskStore};

pub async fn run(store: Arc<dyn TaskStore>, request: ReapRequest, every: std::time::Duration) {
    let mut interval = tokio::time::interval(every);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        match store.reap_expired(request).await {
            Ok(outcome) if outcome.requeued > 0 || outcome.abandoned > 0 => {
                tracing::info!(requeued = outcome.requeued, abandoned = outcome.abandoned, "reaped expired leases");
            }
            Ok(_) => {}
            Err(error) => {
                // Transient by assumption: the next tick retries. Ending
                // the loop here would silently disable retry-on-death.
                tracing::warn!(%error, "reap pass failed; will retry next interval");
            }
        }
    }
}
```

Add `pub mod reaper;` to `lib.rs`.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p oxo-control reaper`
Expected: PASS, with no wall-clock time elapsed (paused time).

- [ ] **Step 4: `make verify`, then commit**

```bash
git add oxo-control/src/reaper.rs oxo-control/src/lib.rs
git commit -m "feat(control): the reaper loop that drives lease expiry

Calls reap_expired on a fixed interval, logs passes that reclaimed
anything, and survives adapter errors — a transient database outage
must not end lease enforcement. Tested under paused tokio time with
the store's TestClock advanced in step.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 11: `oxo-controld` — configuration and the composition root

**Files:**
- Modify: `Cargo.toml` (workspace members)
- Create: `oxo-controld/Cargo.toml`
- Create: `oxo-controld/src/main.rs`
- Create: `oxo-controld/src/config.rs`

**Interfaces:**
- Produces: the `oxo-controld` binary. `Config` (clap derive): `--bind`/`OXO_BIND` default `127.0.0.1:8080`; `--database-url`/`DATABASE_URL` required; `--heartbeat-timeout-secs`/`OXO_HEARTBEAT_TIMEOUT_SECS` default `120`; `--max-task-duration-secs`/`OXO_MAX_TASK_DURATION_SECS` default `21600`; `--reap-interval-secs`/`OXO_REAP_INTERVAL_SECS` default `30`. `Config::reap_request() -> Result<ReapRequest, InvalidQuantity>` validates through `TimeoutSeconds` so a zero bound is refused before the server binds.
- Consumes: `oxo_control::{api::router, reaper}`, `oxo_tasks_postgres::{PostgresTaskStore, run_migrations}`, `oxo_tasks::SystemClock` (match the real exported path).

- [ ] **Step 1: Crate and failing config tests**

Root `Cargo.toml` members gains `"oxo-controld"`. `oxo-controld/Cargo.toml`:

```toml
[package]
name = "oxo-controld"
description = "OXO control plane daemon: serves the claim API over the PostgreSQL adapter"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
oxo-control = { path = "../oxo-control" }
oxo-tasks = { path = "../oxo-tasks" }
oxo-tasks-postgres = { path = "../oxo-tasks-postgres" }
axum = { workspace = true }
clap = { workspace = true }
sqlx = { workspace = true }
tokio = { workspace = true }
tower-http = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

`config.rs` tests first:

```rust
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
            "oxo-controld", "--database-url", "postgres://x", "--heartbeat-timeout-secs", "0",
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
}
```

- [ ] **Step 2: Implement config**

```rust
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
}
```

(clap needs its `env` feature: amend the workspace clap line to `features = ["derive", "env"]`.)

- [ ] **Step 3: The composition root**

`main.rs`:

```rust
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
```

(Match the real exported paths for `SystemClock` and `TaskStore` — read `oxo-tasks/src/lib.rs`'s re-exports. If `SystemClock` is a unit struct behind a different constructor, use the form the existing adapters' binaries/tests use.)

- [ ] **Step 4: Run and verify**

Run: `cargo test -p oxo-controld` (config tests PASS) and `cargo build -p oxo-controld` (the wiring compiles). Then `./target/debug/oxo-controld --help` and confirm the flags render with their env names and defaults. Do not start it against a database — `make verify-db` covers the adapter, and the daemon's wiring is exactly the lines above.

- [ ] **Step 5: `make verify`, then commit**

```bash
git add Cargo.toml oxo-controld
git commit -m "feat(controld): the composition root

Configuration with env fallbacks, reap bounds validated through
TimeoutSeconds at startup so a zero bound is refused before the server
binds, migrations run on boot, the reaper spawned beside the server.
The only new code that knows PostgreSQL exists.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 12: Gherkin acceptance

**Files:**
- Create: `oxo-control/features/control_plane.feature`
- Create: `oxo-control/tests/acceptance.rs`
- Modify: `oxo-control/Cargo.toml` (dev-dependencies: `cucumber = "0.20.2"`)

**Interfaces:**
- Consumes: the full router over the in-memory store — scenarios drive real HTTP requests through `tower::ServiceExt::oneshot`, no network, no sleeps.

- [ ] **Step 1: The feature file**

```gherkin
Feature: Region production through the control plane

  The operator submits a validated region specification; worker pods
  pull tasks, report outcomes, and the completion gate answers.

  Scenario: A region is produced to completion
    Given a control plane with an empty store
    When the operator submits a specification for region "NA" with tiles "+50-002" and "+51-002" including overlays
    Then the submission is accepted with one ortho and one overlay task per tile
    When workers claim and complete every task
    Then the job reports complete

  Scenario: A tile that exhausts its attempts fails the job
    Given a control plane with an empty store
    When the operator submits a specification for region "NA" with tiles "+50-002" including overlays disabled, two attempts and no backoff
    And a worker claims the task and reports failure with reason "Crash!" until it is abandoned
    Then the job reports failed with one abandoned task

  Scenario: Resubmitting an unchanged specification resumes the job
    Given a control plane with an empty store
    When the operator submits a specification for region "NA" with tiles "+50-002" and "+51-002" including overlays
    And the operator submits the same specification again
    Then the second submission resumes the existing job rather than creating a new one
```

- [ ] **Step 2: The steps**

`tests/acceptance.rs`: a cucumber `World` holding the `Router`, the last response's status and parsed JSON body, the submitted spec text, and the collected claim results. Steps build the specification TOML from the scenario's parameters (same canonical shape as Task 8's `SPEC` constant), POST/GET via `oneshot`, and assert. "Workers claim and complete every task" loops claim → complete until a claim answers `204`, completing each task via its returned `lease_token`; the per-tile assertion checks each expected `(tile, task_type)` pair was seen exactly once. The failure scenario loops claim → fail and asserts the final fail body is `{"outcome": "abandoned"}` before checking the gate. Mirror `oxo-spec/tests/acceptance.rs` for the cucumber harness shape (`World::run` entry point, tokio main).

- [ ] **Step 3: Run the scenarios**

Run: `cargo test -p oxo-control --test acceptance`
Expected: every scenario passes. Then run once with a deliberately broken step assertion (e.g. expect `complete` where the failure scenario ends `failed`) to show the harness bites; restore.

- [ ] **Step 4: `make verify`, then commit**

```bash
git add oxo-control/features oxo-control/tests oxo-control/Cargo.toml
git commit -m "test(control): acceptance — the control plane end to end

Three scenarios through the real router: production to completion
across both task types, a tile that exhausts its budget failing the
job, and an unchanged resubmission resuming rather than duplicating.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 13: True up the documentation

**Files:**
- Modify: `CLAUDE.md`
- Modify: `docs/specs/2026-10-01-job-server-design.md`

**Interfaces:** none — prose only, and `make verify` is not required for docs-only changes (house rule), though the numbers transcribed must come from running it.

- [ ] **Step 1: CLAUDE.md**

- Add `oxo-control` and `oxo-controld` rows to the crate table, one line each, matching the design document's descriptions.
- Update the "Currently passing" paragraph by **running** `make verify` and `make verify-db` and transcribing the real totals — never editing the old numbers arithmetically.
- Add one line to the commands section: the daemon runs as `cargo run -p oxo-controld -- --database-url …` and `--help` lists its flags and env fallbacks.

- [ ] **Step 2: Mark the settled section implemented**

In `docs/specs/2026-10-01-job-server-design.md`, add one sentence at the top of "Settled for sub-project 3, not open": `**Implemented by sub-project 3** (see docs/specs/2026-10-02-control-plane-design.md, which records one narrowing: zero stays legal for backoff).`

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md docs/specs/2026-10-01-job-server-design.md
git commit -m "docs: true up CLAUDE.md and mark the settled questions implemented

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
