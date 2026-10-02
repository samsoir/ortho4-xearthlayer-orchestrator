# Job Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A durable job store that hands each unit of tile production to exactly one worker, reclaims work from workers that die, retries under a declared policy, and can state whether a region is finished — behind a port with an in-memory and a PostgreSQL adapter.

**Architecture:** Two crates. `oxo-jobs` carries the port trait, its owned request/response types, the job state machine and an in-memory adapter, with no database dependency at all. `oxo-jobs-postgres` carries the PostgreSQL adapter. One conformance suite, shipped from `oxo-jobs` behind a feature flag, runs against both adapters so the in-memory one cannot drift into a convenient fiction.

**Tech Stack:** Rust (edition 2021), `async-trait` for a dyn-compatible port, `tokio` for the runtime, `chrono` for timestamps, `uuid` for identities, `sqlx` (PostgreSQL, in the adapter crate only), `thiserror` for error types, podman for a disposable test database.

**Spec:** `docs/specs/2026-10-01-job-server-design.md` (and the architecture it inherits, `docs/specs/2026-10-01-oxo-architecture-design.md`)

## Global Constraints

Every task's requirements implicitly include this section.

- **TDD is mandatory.** Write the failing test, run it and capture the real failing output, then write the minimal code to pass. A report without per-cycle RED output is incomplete.
- **SOLID.** This sub-project is where the project's dependency-inversion constraint finally has something real to invert: consumers depend on the `JobStore` trait, never on a concrete adapter.
- **Rust for all code.** Test coverage target 80% minimum, 90%+ aspirational; not measured in CI, which does not yet exist.
- **`clippy` with `-D warnings` and tests with `RUSTFLAGS="-D warnings"`** via `make verify`, so warnings are build failures and output must be pristine.
- **`oxo-jobs` must not depend on `sqlx`, any database driver, or any network or filesystem crate.** The whole point of the split is that a consumer can depend on the port without the adapter's dependencies entering its graph. A task that adds such a dependency to `oxo-jobs` has broken the design.
- **Every timestamp comes from an injected `Clock`.** No `SystemTime::now()` or `Utc::now()` anywhere outside `SystemClock`, and the PostgreSQL adapter never calls SQL `now()` — timestamps travel as query parameters. This is what makes expiry deterministically testable against both adapters and removes application-versus-database clock skew.
- **Every port method takes an owned request and returns an owned response, with no transaction spanning a call.** This keeps a future network adapter mechanical. It rules out exposing `begin → select → update → commit` across the API.
- **A lease token is required by `heartbeat`, `complete` and `fail`,** and a mismatch is `JobStoreError::LeaseLost` — never silently ignored, never a generic error.
- **`attempts` counts starts, not failures.** It increments at claim time. `max_attempts` bounds starts.
- **A reaped job takes the same backoff as a reported failure.**
- **Commit messages end with** `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`, preceded by a blank line or git parses no trailer. Verify with `git log -1 --format='%s'` (subject alone) and `git log -1 --format='%(trailers)'`.
- **After committing, confirm what you committed.** `git status --porcelain` must be empty and `git show HEAD:<path>` must contain your change. A task in the previous sub-project passed `make verify` against its working tree while the commit held none of it.

### Vocabulary

| Term | Meaning |
|---|---|
| Run | One submission of one specification revision. Identified by `(region_code, revision)`. |
| Job | One unit of work: one tile, one job type, within one run. |
| Job type | `Ortho` or `Overlay`. A tile may have both; they are independent jobs. |
| Lease | The right to work on a claimed job, carried by a token minted at claim. |
| Attempt | A start. Incremented when a job is claimed, never when it fails. |
| Reap | Reclaiming a claimed job whose heartbeat lapsed or which exceeded the maximum duration. |

---

### Task 1: `oxo-jobs` crate, identities, and the clock

**Files:**
- Modify: `Cargo.toml`
- Create: `oxo-jobs/Cargo.toml`
- Create: `oxo-jobs/src/lib.rs`
- Create: `oxo-jobs/src/clock.rs`
- Create: `oxo-jobs/src/ids.rs`

**Interfaces:**
- Produces: `Clock` (trait, `fn now(&self) -> DateTime<Utc>`), `SystemClock`, `TestClock` (`new`, `advance`); `RunId`, `JobId`, `LeaseToken` — each a `Uuid` newtype with `generate()`, `from_uuid()`, `as_uuid()`, `Display`, and `Copy + Eq + Hash + Ord`.

- [ ] **Step 1: Add the crate to the workspace**

Modify `Cargo.toml` so `[workspace]` and `[workspace.dependencies]` read:

```toml
[workspace]
members = ["oxo-spec", "oxo-spec-cli", "oxo-jobs"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"
rust-version = "1.74"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
toml = "0.8"
clap = { version = "4", features = ["derive"] }
async-trait = "0.1"
chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }
thiserror = "2"
uuid = { version = "1", features = ["v4"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time"] }
```

Create `oxo-jobs/Cargo.toml`:

```toml
[package]
name = "oxo-jobs"
description = "Job store port, state machine and in-memory adapter for the Ortho4 XEarthLayer Orchestrator"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
async-trait = { workspace = true }
chrono = { workspace = true }
oxo-spec = { path = "../oxo-spec" }
thiserror = { workspace = true }
uuid = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }
```

If `cargo` reports that `chrono`'s `clock` feature pulls something unwanted, keep the feature — `SystemClock` needs it — and say so in your report rather than removing it.

- [ ] **Step 2: Write the failing tests**

Create `oxo-jobs/src/clock.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::Duration;

    #[test]
    fn a_test_clock_does_not_move_on_its_own() {
        let clock = TestClock::new(Utc.timestamp_opt(1_700_000_000, 0).unwrap());
        let first = clock.now();
        let second = clock.now();
        assert_eq!(first, second);
    }

    #[test]
    fn a_test_clock_advances_exactly_as_asked() {
        let start = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let clock = TestClock::new(start);
        clock.advance(Duration::from_secs(90));
        assert_eq!(clock.now(), start + chrono::Duration::seconds(90));
    }

    #[test]
    fn the_system_clock_moves_forward() {
        let clock = SystemClock;
        let first = clock.now();
        let second = clock.now();
        assert!(second >= first);
    }
}
```

Create `oxo-jobs/src/ids.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identities_are_distinct() {
        assert_ne!(RunId::generate(), RunId::generate());
        assert_ne!(JobId::generate(), JobId::generate());
        assert_ne!(LeaseToken::generate(), LeaseToken::generate());
    }

    #[test]
    fn an_identity_round_trips_through_its_uuid() {
        let id = JobId::generate();
        assert_eq!(JobId::from_uuid(id.as_uuid()), id);
    }

    #[test]
    fn an_identity_displays_as_its_uuid() {
        let id = RunId::generate();
        assert_eq!(id.to_string(), id.as_uuid().to_string());
    }

    #[test]
    fn identities_of_different_kinds_are_different_types() {
        // A compile-time property, asserted by construction: this function
        // would not compile if JobId and RunId were the same type.
        fn takes_job(_: JobId) {}
        takes_job(JobId::generate());
    }
}
```

Create `oxo-jobs/src/lib.rs`:

```rust
//! Job store port, state machine and in-memory adapter for the Ortho4
//! XEarthLayer Orchestrator.
//!
//! This crate has no database dependency. Consumers depend on the
//! [`JobStore`] port; a concrete adapter is named only at the composition
//! root. Every timestamp comes from an injected [`Clock`], so expiry and
//! backoff are deterministically testable and the application never
//! disagrees with a database about what time it is.

#![forbid(unsafe_code)]

pub mod clock;
pub mod ids;

pub use clock::{Clock, SystemClock, TestClock};
pub use ids::{JobId, LeaseToken, RunId};
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --package oxo-jobs`
Expected: FAIL to compile — `cannot find type TestClock`, `cannot find type RunId`.

- [ ] **Step 4: Implement the clock**

Prepend to `oxo-jobs/src/clock.rs`:

```rust
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Source of time for the job store.
///
/// Injected rather than read from the host so that expiry, backoff and the
/// maximum-duration backstop are deterministically testable, and so the
/// application and its database never disagree about the current instant.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

/// The host clock. The only place in this crate that reads real time.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock a test drives by hand. Never moves unless advanced.
#[derive(Debug)]
pub struct TestClock {
    now: Mutex<DateTime<Utc>>,
}

impl TestClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    /// Move time forward. Panics only if a previous holder of the lock
    /// panicked, which in a test is the failure you want surfaced.
    pub fn advance(&self, by: Duration) {
        let step = chrono::Duration::from_std(by).expect("advance fits in chrono::Duration");
        let mut now = self.now.lock().expect("test clock lock poisoned");
        *now += step;
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("test clock lock poisoned")
    }
}
```

- [ ] **Step 5: Implement the identities**

Prepend to `oxo-jobs/src/ids.rs`:

```rust
use std::fmt;

use uuid::Uuid;

macro_rules! identity {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint a fresh, random identity.
            pub fn generate() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

identity!(RunId, "Identifies one run: one submission of one specification revision.");
identity!(JobId, "Identifies one job: one tile, one job type, within one run.");
identity!(
    LeaseToken,
    "Proves the right to report on a claimed job. Minted fresh at every claim, so a worker whose job was reclaimed cannot report on it."
);
```

A macro is used here rather than three hand-written copies because the three types are identical in every respect except their name and documentation; writing them out would be the verbatim duplication the review rubric treats as a defect.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --package oxo-jobs`
Expected: PASS, 7 tests.

- [ ] **Step 7: Verify the dependency boundary**

Run: `cargo tree --package oxo-jobs --edges normal | grep -iE "sqlx|postgres|tokio-postgres|reqwest|hyper"`
Expected: no output. If anything matches, a database or network dependency has entered `oxo-jobs` and the design is broken — stop and report it.

- [ ] **Step 8: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add Cargo.toml Cargo.lock oxo-jobs/
git commit -F - <<'EOF'
feat(jobs): add the oxo-jobs crate, its identities and an injected clock

Starts the job server with the two pieces every later task needs: three
Uuid newtypes that cannot be confused with one another, and a Clock trait
so that expiry, backoff and the maximum-duration backstop are testable
without sleeping and the application never disagrees with its database
about the current instant.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 2: Domain types and the port's request/response surface

**Files:**
- Create: `oxo-jobs/src/job.rs`
- Create: `oxo-jobs/src/request.rs`
- Create: `oxo-jobs/src/error.rs`
- Modify: `oxo-jobs/src/lib.rs`

**Interfaces:**
- Consumes: `JobId`, `RunId`, `LeaseToken`, `oxo_spec::TileId`.
- Produces: `JobType { Ortho, Overlay }`; `JobState { Pending, Claimed, Succeeded, Abandoned }`; `JobSpec { tile, job_type }`; `CreateRun { region_code, revision, max_attempts, backoff, jobs }`; `RunCreated { run_id, created, total_jobs }`; `ClaimRequest { worker, job_types }`; `ClaimedJob { job_id, run_id, lease, tile, job_type, attempt }`; `Lease { job_id, token }`; `FailRequest { lease, reason }`; `FailOutcome { Requeued { claimable_at, attempts_remaining }, Abandoned }`; `ReapRequest { heartbeat_timeout, max_job_duration }`; `ReapOutcome { requeued, abandoned }`; `RunStatus { Complete, Failed { abandoned }, InProgress { pending, claimed, succeeded, abandoned } }`; `Throughput { pending, claimable_now, claimed, succeeded, abandoned }`; `JobStoreError`.

- [ ] **Step 1: Write the failing tests**

Create `oxo-jobs/src/job.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_types_are_distinct_and_namable() {
        assert_ne!(JobType::Ortho, JobType::Overlay);
        assert_eq!(JobType::Ortho.as_str(), "ortho");
        assert_eq!(JobType::Overlay.as_str(), "overlay");
    }

    #[test]
    fn job_type_round_trips_through_its_string_form() {
        for job_type in [JobType::Ortho, JobType::Overlay] {
            assert_eq!(JobType::from_str_exact(job_type.as_str()), Some(job_type));
        }
        assert_eq!(JobType::from_str_exact("ORTHO"), None);
        assert_eq!(JobType::from_str_exact("mesh"), None);
    }

    #[test]
    fn terminal_states_are_marked_as_such() {
        assert!(!JobState::Pending.is_terminal());
        assert!(!JobState::Claimed.is_terminal());
        assert!(JobState::Succeeded.is_terminal());
        assert!(JobState::Abandoned.is_terminal());
    }

    #[test]
    fn job_state_round_trips_through_its_string_form() {
        for state in [
            JobState::Pending,
            JobState::Claimed,
            JobState::Succeeded,
            JobState::Abandoned,
        ] {
            assert_eq!(JobState::from_str_exact(state.as_str()), Some(state));
        }
        assert_eq!(JobState::from_str_exact("running"), None);
    }
}
```

Create `oxo-jobs/src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::JobId;

    #[test]
    fn a_lost_lease_says_so_and_names_the_job() {
        let job_id = JobId::generate();
        let error = JobStoreError::LeaseLost { job_id };
        let rendered = error.to_string();
        assert!(rendered.contains("lease"), "{rendered}");
        assert!(rendered.contains(&job_id.to_string()), "{rendered}");
    }

    #[test]
    fn a_run_conflict_names_the_identity_that_collided() {
        let error = JobStoreError::RunConflict {
            region_code: "NA".to_string(),
            revision: 2,
        };
        let rendered = error.to_string();
        assert!(rendered.contains("NA"), "{rendered}");
        assert!(rendered.contains('2'), "{rendered}");
    }

    #[test]
    fn every_variant_renders_something_an_operator_can_act_on() {
        let job_id = JobId::generate();
        let run_id = crate::ids::RunId::generate();
        let cases: Vec<(JobStoreError, &str)> = vec![
            (JobStoreError::LeaseLost { job_id }, "lease"),
            (JobStoreError::UnknownRun { run_id }, "run"),
            (JobStoreError::UnknownJob { job_id }, "job"),
            (JobStoreError::NotClaimed { job_id }, "not claimed"),
            (
                JobStoreError::RunConflict {
                    region_code: "OC".to_string(),
                    revision: 1,
                },
                "already exists",
            ),
            (
                JobStoreError::Adapter("connection reset".to_string()),
                "connection reset",
            ),
        ];
        for (error, fragment) in cases {
            let rendered = error.to_string();
            assert!(
                rendered.contains(fragment),
                "{error:?} rendered {rendered:?}, expected it to contain {fragment:?}"
            );
        }
    }
}
```

Create `oxo-jobs/src/request.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tile(lat: i8, lon: i16) -> TileId {
        TileId::new(lat, lon).expect("in range")
    }

    #[test]
    fn a_create_run_request_carries_its_whole_job_set() {
        let request = CreateRun {
            region_code: "NA".to_string(),
            revision: 1,
            max_attempts: 3,
            backoff: Duration::from_secs(60),
            jobs: vec![
                JobSpec {
                    tile: tile(50, -2),
                    job_type: JobType::Ortho,
                },
                JobSpec {
                    tile: tile(50, -2),
                    job_type: JobType::Overlay,
                },
            ],
        };
        assert_eq!(request.jobs.len(), 2);
        assert_eq!(request.jobs[0].tile, request.jobs[1].tile);
        assert_ne!(request.jobs[0].job_type, request.jobs[1].job_type);
    }

    #[test]
    fn a_claim_request_with_no_filter_means_any_job_type() {
        let request = ClaimRequest {
            worker: "pod-1".to_string(),
            job_types: None,
        };
        assert!(request.job_types.is_none());
    }

    #[test]
    fn a_fail_outcome_distinguishes_requeued_from_abandoned() {
        let requeued = FailOutcome::Requeued {
            claimable_at: chrono::Utc::now(),
            attempts_remaining: 2,
        };
        let abandoned = FailOutcome::Abandoned;
        assert_ne!(
            std::mem::discriminant(&requeued),
            std::mem::discriminant(&abandoned)
        );
    }

    #[test]
    fn a_run_status_distinguishes_its_three_shapes() {
        let complete = RunStatus::Complete;
        let failed = RunStatus::Failed { abandoned: 1 };
        let in_progress = RunStatus::InProgress {
            pending: 1,
            claimed: 0,
            succeeded: 0,
            abandoned: 0,
        };
        assert_ne!(complete, failed);
        assert_ne!(failed, in_progress);
        assert_ne!(complete, in_progress);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-jobs`
Expected: FAIL to compile — `JobType`, `JobStoreError`, `CreateRun` not found.

- [ ] **Step 3: Implement the domain types**

Prepend to `oxo-jobs/src/job.rs`:

```rust
/// The two independent kinds of work a tile needs.
///
/// They are separate jobs because they share no data, their resource
/// profiles differ by orders of magnitude, and their dependencies are
/// disjoint — an imagery provider and Overpass for ortho, an X-Plane
/// overlay source and DSFTool for overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum JobType {
    Ortho,
    Overlay,
}

impl JobType {
    /// The canonical lowercase name, used as the database enum label.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ortho => "ortho",
            Self::Overlay => "overlay",
        }
    }

    /// Parse the canonical form. Exact match only — no case folding, so a
    /// database label and this enum cannot drift apart silently.
    pub fn from_str_exact(text: &str) -> Option<Self> {
        match text {
            "ortho" => Some(Self::Ortho),
            "overlay" => Some(Self::Overlay),
            _ => None,
        }
    }

    /// Every variant, for exhaustive iteration in tests and queries.
    pub const ALL: [JobType; 2] = [JobType::Ortho, JobType::Overlay];
}

/// Where a job is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JobState {
    /// Claimable once `claimable_at` has passed.
    Pending,
    /// Held under a lease, expected to heartbeat.
    Claimed,
    Succeeded,
    /// Retries exhausted. The run can never complete.
    Abandoned,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Succeeded => "succeeded",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn from_str_exact(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "succeeded" => Some(Self::Succeeded),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Abandoned)
    }
}
```

- [ ] **Step 4: Implement the error type**

Prepend to `oxo-jobs/src/error.rs`:

```rust
use thiserror::Error;

use crate::ids::{JobId, RunId};

/// Why a job store operation did not succeed.
///
/// The first five are deterministic outcomes of a correct store and must be
/// representable without a caller parsing a string. Only [`Adapter`] may be
/// worth retrying.
///
/// [`Adapter`]: JobStoreError::Adapter
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum JobStoreError {
    /// The lease token does not match the one the job was claimed with. The
    /// caller has lost this job — another worker may already hold it — and
    /// must stop rather than retry.
    #[error("lease for job {job_id} is no longer held; it was reclaimed and must not be reported on")]
    LeaseLost { job_id: JobId },

    #[error("no such run {run_id}")]
    UnknownRun { run_id: RunId },

    #[error("no such job {job_id}")]
    UnknownJob { job_id: JobId },

    #[error("job {job_id} is not claimed, so it cannot be completed or failed")]
    NotClaimed { job_id: JobId },

    /// A run for this identity already exists with a different job set or
    /// policy. Resuming it would silently run something other than what was
    /// asked for.
    #[error(
        "a run for region {region_code} revision {revision} already exists with a different job set or failure policy"
    )]
    RunConflict { region_code: String, revision: u32 },

    #[error("job store adapter failed: {0}")]
    Adapter(String),
}
```

- [ ] **Step 5: Implement the request and response types**

Prepend to `oxo-jobs/src/request.rs`:

```rust
use std::time::Duration;

use chrono::{DateTime, Utc};
use oxo_spec::TileId;

use crate::ids::{JobId, LeaseToken, RunId};
use crate::job::JobType;

/// One unit of work within a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    pub tile: TileId,
    pub job_type: JobType,
}

/// Register a run and the whole job set it consists of.
///
/// The job set arrives already computed: atomizing a specification into up
/// to 2N jobs is the planner's work, not the store's. The failure policy is
/// snapshotted onto the run, so editing a specification later cannot change
/// the policy of a run already in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRun {
    pub region_code: String,
    pub revision: u32,
    pub max_attempts: u32,
    pub backoff: Duration,
    pub jobs: Vec<JobSpec>,
}

/// The outcome of registering a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunCreated {
    pub run_id: RunId,
    /// `false` when a run for this identity already existed and this call
    /// resumed it rather than creating anything.
    pub created: bool,
    pub total_jobs: u32,
}

/// Ask for one job to work on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRequest {
    /// Worker identity, recorded so an operator can see who holds what.
    pub worker: String,
    /// Restrict to these job types. `None` means any.
    ///
    /// This is how a worker expresses capacity until there is a footprint
    /// model: one short on disk claims overlay work only, an overlay job
    /// being a file copy and a conversion where an ortho job is hundreds of
    /// gigabytes.
    pub job_types: Option<Vec<JobType>>,
}

/// A job handed to a worker, with the lease that proves it is theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    pub job_id: JobId,
    pub run_id: RunId,
    pub lease: LeaseToken,
    pub tile: TileId,
    pub job_type: JobType,
    /// Which start this is, counting from 1.
    pub attempt: u32,
}

/// Proof that the caller holds a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub job_id: JobId,
    pub token: LeaseToken,
}

/// Report that a job failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailRequest {
    pub lease: Lease,
    /// Whatever the worker could determine. Recorded, never interpreted —
    /// Ortho4XP's headless path reports a bare `Crash!`, so the store draws
    /// no conclusions from this text.
    pub reason: String,
}

/// What became of a failed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    Requeued {
        claimable_at: DateTime<Utc>,
        attempts_remaining: u32,
    },
    Abandoned,
}

/// Reclaim jobs whose workers have gone away.
///
/// Both bounds are server configuration rather than per-run or
/// per-specification: they are operational tuning, and an operator
/// authoring a region is the person least placed to choose them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReapRequest {
    /// Reclaim a claimed job if no heartbeat has arrived within this.
    pub heartbeat_timeout: Duration,
    /// Reclaim a claimed job once it has been held this long regardless of
    /// heartbeats, covering a worker that is wedged but alive.
    pub max_job_duration: Duration,
}

/// What a reap did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReapOutcome {
    pub requeued: u32,
    /// Reclaimed jobs whose attempts were already exhausted.
    pub abandoned: u32,
}

/// Whether a region is finished — the completion gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Complete,
    /// Nothing left to run, and at least one job abandoned. The region can
    /// never complete.
    Failed { abandoned: u32 },
    /// Work remains. `abandoned` is reported here too, deliberately: a run
    /// with an abandoned job is already unachievable, and an operator should
    /// learn that in minutes rather than after a fortnight.
    InProgress {
        pending: u32,
        claimed: u32,
        succeeded: u32,
        abandoned: u32,
    },
}

/// A snapshot of a run's queue.
///
/// Counts, not rates: rates need two observations, and differencing
/// successive snapshots is the consumer's job. The metric set is an open
/// decision in the design document, to be settled with the first real
/// consumer rather than guessed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Throughput {
    pub pending: u32,
    /// Pending jobs whose backoff has elapsed, so claimable right now.
    pub claimable_now: u32,
    pub claimed: u32,
    pub succeeded: u32,
    pub abandoned: u32,
}
```

- [ ] **Step 6: Wire the modules**

Add to `oxo-jobs/src/lib.rs`, keeping alphabetical grouping:

```rust
pub mod error;
pub mod job;
pub mod request;

pub use error::JobStoreError;
pub use job::{JobState, JobType};
pub use request::{
    ClaimRequest, ClaimedJob, CreateRun, FailOutcome, FailRequest, JobSpec, Lease, ReapOutcome,
    ReapRequest, RunCreated, RunStatus, Throughput,
};
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --package oxo-jobs`
Expected: PASS, 15 tests.

- [ ] **Step 8: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add oxo-jobs/
git commit -F - <<'EOF'
feat(jobs): add the domain types and the port's request surface

Job types and states with exact string forms, so a database enum label
and the Rust enum cannot drift apart silently. Owned request and response
types for every port method, which is what keeps a future network adapter
mechanical rather than a redesign.

JobStoreError distinguishes the five deterministic outcomes of a correct
store from an adapter failure, so a caller never parses a string to learn
whether retrying is sensible. LeaseLost says outright that the caller has
lost the job and must stop.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 3: The `JobStore` port

**Files:**
- Create: `oxo-jobs/src/store.rs`
- Modify: `oxo-jobs/src/lib.rs`

**Interfaces:**
- Consumes: every type from Task 2.
- Produces: `#[async_trait] pub trait JobStore: Send + Sync` with the eight methods below, and a compile-time assertion that it is dyn-compatible.

- [ ] **Step 1: Write the failing test**

Create `oxo-jobs/src/store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The composition root injects an adapter behind a trait object, so
    /// losing dyn-compatibility would break the design. This fails to
    /// compile rather than at runtime if that happens.
    #[test]
    fn the_port_is_dyn_compatible() {
        fn assert_dyn_compatible(_: Option<&dyn JobStore>) {}
        assert_dyn_compatible(None);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --package oxo-jobs store`
Expected: FAIL to compile — `cannot find trait JobStore`.

- [ ] **Step 3: Implement the port**

Prepend to `oxo-jobs/src/store.rs`:

```rust
use async_trait::async_trait;

use crate::error::JobStoreError;
use crate::ids::RunId;
use crate::request::{
    ClaimRequest, ClaimedJob, CreateRun, FailOutcome, FailRequest, Lease, ReapOutcome, ReapRequest,
    RunCreated, RunStatus, Throughput,
};

/// Durable job state, leasing, retry accounting and the completion gate.
///
/// Every method takes an owned request and returns an owned response, and
/// no transaction spans a call. That is deliberate: it keeps a network
/// adapter a mechanical addition rather than a redesign, and it rules out
/// exposing `begin → select → update → commit` across this boundary.
///
/// `async_trait` is used so the port is dyn-compatible — the composition
/// root injects an adapter behind a trait object.
#[async_trait]
pub trait JobStore: Send + Sync {
    /// Register a run and its job set. Idempotent on
    /// `(region_code, revision)`: calling it again with the same job set
    /// and policy resumes the existing run and reports `created: false`.
    /// Calling it with the same identity but a different job set or policy
    /// is [`JobStoreError::RunConflict`].
    async fn create_run(&self, request: CreateRun) -> Result<RunCreated, JobStoreError>;

    /// Hand one claimable job to a worker, minting a fresh lease token.
    /// `Ok(None)` means nothing is claimable, which is not an error.
    ///
    /// Increments the job's attempt count. Attempts count starts, so a job
    /// reclaimed from a dead worker has already consumed one.
    async fn claim(&self, request: ClaimRequest) -> Result<Option<ClaimedJob>, JobStoreError>;

    /// Assert that a lease is still held. Returns
    /// [`JobStoreError::LeaseLost`] if the job was reclaimed, which tells
    /// the worker to stop working.
    async fn heartbeat(&self, lease: Lease) -> Result<(), JobStoreError>;

    /// Mark a claimed job succeeded.
    async fn complete(&self, lease: Lease) -> Result<(), JobStoreError>;

    /// Record a failure, requeueing after backoff or abandoning if the
    /// attempt budget is spent.
    async fn fail(&self, request: FailRequest) -> Result<FailOutcome, JobStoreError>;

    /// Reclaim claimed jobs whose heartbeat has lapsed or which have
    /// exceeded the maximum duration. A reaped job takes the same backoff
    /// as a reported failure, so a tile that kills its worker cannot be
    /// re-claimed instantly and burn its budget in minutes.
    async fn reap_expired(&self, request: ReapRequest) -> Result<ReapOutcome, JobStoreError>;

    /// The completion gate.
    async fn run_status(&self, run_id: RunId) -> Result<RunStatus, JobStoreError>;

    /// A snapshot of the run's queue.
    async fn throughput(&self, run_id: RunId) -> Result<Throughput, JobStoreError>;
}
```

- [ ] **Step 4: Wire the module**

Add to `oxo-jobs/src/lib.rs`:

```rust
pub mod store;

pub use store::JobStore;
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --package oxo-jobs store`
Expected: PASS, 1 test.

- [ ] **Step 6: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add oxo-jobs/
git commit -F - <<'EOF'
feat(jobs): define the JobStore port

Eight methods, each taking an owned request and returning an owned
response with no transaction spanning a call — the constraint that keeps
a future network adapter mechanical.

async_trait rather than native async-fn-in-trait, both because the
composition root injects an adapter behind a trait object and because
native AFIT stabilised in 1.75, after this workspace's declared 1.74
floor. A test asserts dyn-compatibility at compile time so losing it
fails the build rather than surfacing later.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 4: In-memory adapter — state and `create_run`

**Files:**
- Create: `oxo-jobs/src/memory.rs`
- Modify: `oxo-jobs/src/lib.rs`

**Interfaces:**
- Consumes: `Clock`, every type from Task 2, the `JobStore` trait.
- Produces: `InMemoryJobStore::new(clock: Arc<dyn Clock>)`, implementing `create_run`. Remaining methods are added by Tasks 5-7; until then they return `unimplemented!()` with a comment naming the task that fills them.

- [ ] **Step 1: Write the failing tests**

Create `oxo-jobs/src/memory.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn tile(lat: i8, lon: i16) -> TileId {
        TileId::new(lat, lon).expect("in range")
    }

    pub(crate) fn clock() -> Arc<TestClock> {
        Arc::new(TestClock::new(
            Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        ))
    }

    pub(crate) fn store(clock: Arc<TestClock>) -> InMemoryJobStore {
        InMemoryJobStore::new(clock)
    }

    pub(crate) fn two_tile_run() -> CreateRun {
        CreateRun {
            region_code: "NA".to_string(),
            revision: 1,
            max_attempts: 3,
            backoff: Duration::from_secs(60),
            jobs: vec![
                JobSpec {
                    tile: tile(50, -2),
                    job_type: JobType::Ortho,
                },
                JobSpec {
                    tile: tile(50, -2),
                    job_type: JobType::Overlay,
                },
            ],
        }
    }

    #[tokio::test]
    async fn creating_a_run_reports_what_it_created() {
        let store = store(clock());
        let created = store.create_run(two_tile_run()).await.expect("create");
        assert!(created.created);
        assert_eq!(created.total_jobs, 2);
    }

    #[tokio::test]
    async fn creating_the_same_run_again_resumes_rather_than_duplicating() {
        let store = store(clock());
        let first = store.create_run(two_tile_run()).await.expect("create");
        let second = store.create_run(two_tile_run()).await.expect("resume");

        assert!(first.created);
        assert!(!second.created, "the second call must not have created anything");
        assert_eq!(first.run_id, second.run_id);
        assert_eq!(second.total_jobs, 2);
    }

    #[tokio::test]
    async fn a_different_job_set_under_the_same_identity_is_a_conflict() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.expect("create");

        let mut altered = two_tile_run();
        altered.jobs.push(JobSpec {
            tile: tile(51, -2),
            job_type: JobType::Ortho,
        });

        let error = store.create_run(altered).await.expect_err("should conflict");
        assert!(
            matches!(error, JobStoreError::RunConflict { .. }),
            "expected RunConflict, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_different_failure_policy_under_the_same_identity_is_a_conflict() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.expect("create");

        let mut altered = two_tile_run();
        altered.max_attempts = 5;

        let error = store.create_run(altered).await.expect_err("should conflict");
        assert!(
            matches!(error, JobStoreError::RunConflict { .. }),
            "expected RunConflict, got {error:?}"
        );
    }

    #[tokio::test]
    async fn the_job_set_is_compared_without_regard_to_order() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.expect("create");

        let mut reordered = two_tile_run();
        reordered.jobs.reverse();

        let resumed = store.create_run(reordered).await.expect("resume");
        assert!(!resumed.created, "reordering the job set is not a different run");
    }

    #[tokio::test]
    async fn two_revisions_of_one_region_are_separate_runs() {
        let store = store(clock());
        let first = store.create_run(two_tile_run()).await.expect("create");

        let mut next = two_tile_run();
        next.revision = 2;
        let second = store.create_run(next).await.expect("create");

        assert_ne!(first.run_id, second.run_id);
        assert!(second.created);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-jobs memory`
Expected: FAIL to compile — `cannot find type InMemoryJobStore`.

- [ ] **Step 3: Implement the state and `create_run`**

Prepend to `oxo-jobs/src/memory.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use oxo_spec::TileId;

use crate::clock::{Clock, TestClock};
use crate::error::JobStoreError;
use crate::ids::{JobId, LeaseToken, RunId};
use crate::job::{JobState, JobType};
use crate::request::{
    ClaimRequest, ClaimedJob, CreateRun, FailOutcome, FailRequest, JobSpec, Lease, ReapOutcome,
    ReapRequest, RunCreated, RunStatus, Throughput,
};
use crate::store::JobStore;

/// A job store held entirely in memory.
///
/// Exists so the port can be exercised without a database, and so the
/// conformance suite has a second implementation to hold the PostgreSQL
/// adapter honest. Not durable; not intended for production.
///
/// The mutex is never held across an await — every operation is synchronous
/// once inside it — so a `std` mutex is correct here and simpler than an
/// async one.
#[derive(Debug)]
pub struct InMemoryJobStore {
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    runs: BTreeMap<RunId, Run>,
    by_identity: BTreeMap<(String, u32), RunId>,
    jobs: BTreeMap<JobId, Job>,
}

#[derive(Debug)]
struct Run {
    max_attempts: u32,
    backoff: Duration,
    job_ids: Vec<JobId>,
}

#[derive(Debug)]
struct Job {
    run_id: RunId,
    tile: TileId,
    job_type: JobType,
    state: JobState,
    attempts: u32,
    claimable_at: DateTime<Utc>,
    lease: Option<LeaseToken>,
    claimed_by: Option<String>,
    claimed_at: Option<DateTime<Utc>>,
    last_heartbeat_at: Option<DateTime<Utc>>,
    last_failure: Option<String>,
}

impl InMemoryJobStore {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            state: Mutex::new(State::default()),
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("in-memory store lock poisoned")
    }
}

/// The identity of a job within its run, used to compare job sets.
fn job_key(spec: &JobSpec) -> (TileId, JobType) {
    (spec.tile, spec.job_type)
}

#[async_trait]
impl JobStore for InMemoryJobStore {
    async fn create_run(&self, request: CreateRun) -> Result<RunCreated, JobStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();
        let identity = (request.region_code.clone(), request.revision);

        if let Some(&run_id) = state.by_identity.get(&identity) {
            let run = &state.runs[&run_id];
            let existing: BTreeSet<(TileId, JobType)> = run
                .job_ids
                .iter()
                .map(|id| {
                    let job = &state.jobs[id];
                    (job.tile, job.job_type)
                })
                .collect();
            let requested: BTreeSet<(TileId, JobType)> =
                request.jobs.iter().map(job_key).collect();

            let same_policy =
                run.max_attempts == request.max_attempts && run.backoff == request.backoff;
            if existing != requested || !same_policy {
                return Err(JobStoreError::RunConflict {
                    region_code: request.region_code,
                    revision: request.revision,
                });
            }

            return Ok(RunCreated {
                run_id,
                created: false,
                total_jobs: run.job_ids.len() as u32,
            });
        }

        let run_id = RunId::generate();
        let mut job_ids = Vec::with_capacity(request.jobs.len());
        for spec in &request.jobs {
            let job_id = JobId::generate();
            state.jobs.insert(
                job_id,
                Job {
                    run_id,
                    tile: spec.tile,
                    job_type: spec.job_type,
                    state: JobState::Pending,
                    attempts: 0,
                    claimable_at: now,
                    lease: None,
                    claimed_by: None,
                    claimed_at: None,
                    last_heartbeat_at: None,
                    last_failure: None,
                },
            );
            job_ids.push(job_id);
        }
        let total_jobs = job_ids.len() as u32;
        state.runs.insert(
            run_id,
            Run {
                max_attempts: request.max_attempts,
                backoff: request.backoff,
                job_ids,
            },
        );
        state.by_identity.insert(identity, run_id);

        Ok(RunCreated {
            run_id,
            created: true,
            total_jobs,
        })
    }

    async fn claim(&self, _request: ClaimRequest) -> Result<Option<ClaimedJob>, JobStoreError> {
        unimplemented!("Task 5")
    }

    async fn heartbeat(&self, _lease: Lease) -> Result<(), JobStoreError> {
        unimplemented!("Task 6")
    }

    async fn complete(&self, _lease: Lease) -> Result<(), JobStoreError> {
        unimplemented!("Task 6")
    }

    async fn fail(&self, _request: FailRequest) -> Result<FailOutcome, JobStoreError> {
        unimplemented!("Task 6")
    }

    async fn reap_expired(&self, _request: ReapRequest) -> Result<ReapOutcome, JobStoreError> {
        unimplemented!("Task 7")
    }

    async fn run_status(&self, _run_id: RunId) -> Result<RunStatus, JobStoreError> {
        unimplemented!("Task 7")
    }

    async fn throughput(&self, _run_id: RunId) -> Result<Throughput, JobStoreError> {
        unimplemented!("Task 7")
    }
}
```

The `unimplemented!` stubs are deliberate and temporary: the trait must be fully implemented to compile at all, and Tasks 5-7 replace them. Each names the task that fills it so a stub surviving to the end is obvious.

- [ ] **Step 4: Wire the module**

Add to `oxo-jobs/src/lib.rs`:

```rust
pub mod memory;

pub use memory::InMemoryJobStore;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --package oxo-jobs memory`
Expected: PASS, 6 tests.

- [ ] **Step 6: Run full verification and commit**

Run: `make verify`
Expected: PASS. If clippy objects to `as u32` on a `usize` length, prefer
`u32::try_from(len).unwrap_or(u32::MAX)` and say so in your report — a job
set larger than four billion is not a case worth a fallible signature.

```bash
git add oxo-jobs/
git commit -F - <<'EOF'
feat(jobs): add the in-memory store and run creation

create_run is idempotent on (region_code, revision), which is what makes
resubmitting a specification resume a run rather than duplicate weeks of
work. The job set is compared as a set, so reordering it is not a
different run — but a changed job set or failure policy under the same
identity is a conflict rather than a silent resume, since resuming would
run something other than what was asked for.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 5: In-memory adapter — `claim`

**Files:**
- Modify: `oxo-jobs/src/memory.rs`

**Interfaces:**
- Produces: `claim` replacing its stub. Picks the claimable job with the lowest `(claimable_at, JobId)`, honours the job-type filter, increments `attempts`, mints a fresh `LeaseToken`, records worker and timestamps.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `oxo-jobs/src/memory.rs`:

```rust
    fn any(worker: &str) -> ClaimRequest {
        ClaimRequest {
            worker: worker.to_string(),
            job_types: None,
        }
    }

    #[tokio::test]
    async fn a_claim_hands_out_one_job_with_a_lease() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.expect("create");

        let claimed = store
            .claim(any("pod-1"))
            .await
            .expect("claim")
            .expect("a job was available");
        assert_eq!(claimed.attempt, 1);
    }

    #[tokio::test]
    async fn each_claim_mints_a_distinct_lease_token() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.expect("create");

        let first = store.claim(any("pod-1")).await.unwrap().unwrap();
        let second = store.claim(any("pod-2")).await.unwrap().unwrap();
        assert_ne!(first.job_id, second.job_id);
        assert_ne!(first.lease, second.lease);
    }

    #[tokio::test]
    async fn a_claimed_job_is_not_handed_out_again() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.expect("create");

        store.claim(any("pod-1")).await.unwrap().unwrap();
        store.claim(any("pod-2")).await.unwrap().unwrap();
        let third = store.claim(any("pod-3")).await.unwrap();
        assert!(third.is_none(), "only two jobs exist, so the third claim finds none");
    }

    #[tokio::test]
    async fn an_empty_queue_is_not_an_error() {
        let store = store(clock());
        assert!(store.claim(any("pod-1")).await.expect("claim").is_none());
    }

    #[tokio::test]
    async fn a_job_type_filter_restricts_what_is_handed_out() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.expect("create");

        let claimed = store
            .claim(ClaimRequest {
                worker: "pod-1".to_string(),
                job_types: Some(vec![JobType::Overlay]),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.job_type, JobType::Overlay);

        let again = store
            .claim(ClaimRequest {
                worker: "pod-2".to_string(),
                job_types: Some(vec![JobType::Overlay]),
            })
            .await
            .unwrap();
        assert!(again.is_none(), "only one overlay job exists");
    }

    #[tokio::test]
    async fn a_job_is_not_claimable_before_its_backoff_elapses() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_run(two_tile_run()).await.expect("create");

        // Claim and fail one job so it is requeued with backoff.
        let claimed = store.claim(any("pod-1")).await.unwrap().unwrap();
        store
            .fail(FailRequest {
                lease: Lease {
                    job_id: claimed.job_id,
                    token: claimed.lease,
                },
                reason: "Crash!".to_string(),
            })
            .await
            .expect("fail");

        // The other job is still claimable; take it out of the way.
        store.claim(any("pod-2")).await.unwrap().unwrap();

        assert!(
            store.claim(any("pod-3")).await.unwrap().is_none(),
            "the failed job is in backoff and must not be claimable yet"
        );

        test_clock.advance(Duration::from_secs(60));
        let after_backoff = store.claim(any("pod-3")).await.unwrap();
        assert!(after_backoff.is_some(), "backoff has elapsed, so it is claimable");
        assert_eq!(after_backoff.unwrap().attempt, 2, "a second start");
    }

    #[tokio::test]
    async fn claims_are_handed_out_oldest_claimable_first() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_run(two_tile_run()).await.expect("create");

        // Fail the first job so it is requeued to a later claimable_at,
        // leaving the second job strictly older.
        let first = store.claim(any("pod-1")).await.unwrap().unwrap();
        store
            .fail(FailRequest {
                lease: Lease {
                    job_id: first.job_id,
                    token: first.lease,
                },
                reason: "transient".to_string(),
            })
            .await
            .expect("fail");

        let next = store.claim(any("pod-2")).await.unwrap().unwrap();
        assert_ne!(
            next.job_id, first.job_id,
            "the job still at its original claimable_at must come first"
        );
    }
```

Note that two of these tests call `fail`, which Task 6 implements. They will fail until then — that is expected and correct, because backoff is only observable through a failure. Implement `claim` in this task; the two backoff tests go green in Task 6. **Say so explicitly in your report rather than deleting them or stubbing `fail` to make them pass.**

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-jobs memory`
Expected: FAIL — the five claim tests panic at `unimplemented!("Task 5")`, and the two backoff tests panic at `unimplemented!("Task 6")`.

- [ ] **Step 3: Implement `claim`**

Replace the `claim` stub in `oxo-jobs/src/memory.rs`:

```rust
    async fn claim(&self, request: ClaimRequest) -> Result<Option<ClaimedJob>, JobStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();

        let wanted = request.job_types.as_deref();
        let next = state
            .jobs
            .iter()
            .filter(|(_, job)| job.state == JobState::Pending && job.claimable_at <= now)
            .filter(|(_, job)| wanted.map_or(true, |types| types.contains(&job.job_type)))
            .min_by_key(|(id, job)| (job.claimable_at, **id))
            .map(|(id, _)| *id);

        let Some(job_id) = next else {
            return Ok(None);
        };

        let token = LeaseToken::generate();
        let job = state
            .jobs
            .get_mut(&job_id)
            .expect("the id was just selected from this map");
        job.state = JobState::Claimed;
        job.attempts += 1;
        job.lease = Some(token);
        job.claimed_by = Some(request.worker);
        job.claimed_at = Some(now);
        job.last_heartbeat_at = Some(now);

        Ok(Some(ClaimedJob {
            job_id,
            run_id: job.run_id,
            lease: token,
            tile: job.tile,
            job_type: job.job_type,
            attempt: job.attempts,
        }))
    }
```

`map_or(true, …)` rather than the newer `is_none_or`, which stabilised in
Rust 1.82 — after this workspace's declared 1.74 floor. Clippy honours
`rust-version` and so should not suggest the newer form; if it does anyway,
report that rather than raising the MSRV to satisfy a lint.
`wanted.map_or(true, |types| types.contains(&job.job_type))` and note the
substitution in your report.

- [ ] **Step 4: Run the tests to verify the claim tests pass**

Run: `cargo test --package oxo-jobs memory`
Expected: the five claim tests PASS; the two backoff tests still fail at `unimplemented!("Task 6")`. Report both counts.

- [ ] **Step 5: Commit**

```bash
git add oxo-jobs/
git commit -F - <<'EOF'
feat(jobs): hand out jobs with a lease, oldest claimable first

A claim takes the pending job with the lowest (claimable_at, id), mints a
fresh lease token and increments the attempt count. Attempts count starts
rather than failures, so a job reclaimed from a dead worker has already
consumed one — which is what stops a tile that kills its worker cycling
forever.

The optional job-type filter is how a worker expresses capacity until
there is a footprint model: one short on disk claims overlay work only.

Two backoff tests are present and still failing; backoff is only
observable through a failure, which Task 6 implements.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 6: In-memory adapter — `heartbeat`, `complete`, `fail`

**Files:**
- Modify: `oxo-jobs/src/memory.rs`

**Interfaces:**
- Produces: the three methods replacing their stubs. All three reject a stale lease with `JobStoreError::LeaseLost`, an unclaimed job with `NotClaimed`, and an unknown job with `UnknownJob`. `fail` requeues with backoff while attempts remain and abandons at the limit.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `oxo-jobs/src/memory.rs`:

```rust
    async fn claim_one(store: &InMemoryJobStore) -> ClaimedJob {
        store
            .claim(any("pod-1"))
            .await
            .expect("claim")
            .expect("a job was available")
    }

    fn lease_of(job: &ClaimedJob) -> Lease {
        Lease {
            job_id: job.job_id,
            token: job.lease,
        }
    }

    #[tokio::test]
    async fn a_heartbeat_on_a_held_lease_succeeds() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.unwrap();
        let job = claim_one(&store).await;
        store.heartbeat(lease_of(&job)).await.expect("still held");
    }

    #[tokio::test]
    async fn completing_a_held_job_succeeds_once_and_not_twice() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.unwrap();
        let job = claim_one(&store).await;

        store.complete(lease_of(&job)).await.expect("complete");

        let again = store.complete(lease_of(&job)).await.expect_err("already done");
        assert!(
            matches!(again, JobStoreError::NotClaimed { .. }),
            "expected NotClaimed, got {again:?}"
        );
    }

    #[tokio::test]
    async fn a_stale_lease_is_rejected_by_all_three_reporting_calls() {
        let store = store(clock());
        store.create_run(two_tile_run()).await.unwrap();
        let job = claim_one(&store).await;
        let stale = Lease {
            job_id: job.job_id,
            token: LeaseToken::generate(),
        };

        for error in [
            store.heartbeat(stale).await.expect_err("heartbeat"),
            store.complete(stale).await.expect_err("complete"),
            store
                .fail(FailRequest {
                    lease: stale,
                    reason: "Crash!".to_string(),
                })
                .await
                .expect_err("fail"),
        ] {
            assert!(
                matches!(error, JobStoreError::LeaseLost { .. }),
                "expected LeaseLost, got {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn reporting_on_an_unknown_job_says_so() {
        let store = store(clock());
        let nowhere = Lease {
            job_id: JobId::generate(),
            token: LeaseToken::generate(),
        };
        let error = store.heartbeat(nowhere).await.expect_err("unknown");
        assert!(
            matches!(error, JobStoreError::UnknownJob { .. }),
            "expected UnknownJob, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_failure_with_attempts_remaining_is_requeued_with_backoff() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_run(two_tile_run()).await.unwrap();
        let job = claim_one(&store).await;
        let at_failure = test_clock.now();

        let outcome = store
            .fail(FailRequest {
                lease: lease_of(&job),
                reason: "Crash!".to_string(),
            })
            .await
            .expect("fail");

        match outcome {
            FailOutcome::Requeued {
                claimable_at,
                attempts_remaining,
            } => {
                assert_eq!(claimable_at, at_failure + chrono::Duration::seconds(60));
                assert_eq!(attempts_remaining, 2, "one of three starts is spent");
            }
            FailOutcome::Abandoned => panic!("two attempts remained"),
        }
    }

    #[tokio::test]
    async fn a_job_is_abandoned_on_the_last_permitted_start() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_run(two_tile_run()).await.unwrap();

        // max_attempts is 3, so the third failure abandons.
        for expected_remaining in [2u32, 1] {
            let job = claim_one(&store).await;
            let outcome = store
                .fail(FailRequest {
                    lease: lease_of(&job),
                    reason: "Crash!".to_string(),
                })
                .await
                .expect("fail");
            assert!(
                matches!(
                    outcome,
                    FailOutcome::Requeued { attempts_remaining, .. }
                        if attempts_remaining == expected_remaining
                ),
                "expected {expected_remaining} remaining, got {outcome:?}"
            );
            test_clock.advance(Duration::from_secs(60));
        }

        let last = claim_one(&store).await;
        assert_eq!(last.attempt, 3, "the third and final start");
        let outcome = store
            .fail(FailRequest {
                lease: lease_of(&last),
                reason: "Crash!".to_string(),
            })
            .await
            .expect("fail");
        assert_eq!(outcome, FailOutcome::Abandoned);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-jobs memory`
Expected: FAIL — the new tests and the two backoff tests from Task 5 panic at `unimplemented!("Task 6")`.

- [ ] **Step 3: Implement the three methods**

Add a helper above the `impl JobStore` block in `oxo-jobs/src/memory.rs`:

```rust
impl State {
    /// Resolve a lease to a claimed job, rejecting the three ways it can be
    /// invalid. Shared by heartbeat, complete and fail so the checks cannot
    /// drift apart between them.
    fn claimed_mut(&mut self, lease: Lease) -> Result<&mut Job, JobStoreError> {
        let job = self
            .jobs
            .get_mut(&lease.job_id)
            .ok_or(JobStoreError::UnknownJob {
                job_id: lease.job_id,
            })?;
        if job.state != JobState::Claimed {
            return Err(JobStoreError::NotClaimed {
                job_id: lease.job_id,
            });
        }
        if job.lease != Some(lease.token) {
            return Err(JobStoreError::LeaseLost {
                job_id: lease.job_id,
            });
        }
        Ok(job)
    }
}
```

Then replace the three stubs:

```rust
    async fn heartbeat(&self, lease: Lease) -> Result<(), JobStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();
        let job = state.claimed_mut(lease)?;
        job.last_heartbeat_at = Some(now);
        Ok(())
    }

    async fn complete(&self, lease: Lease) -> Result<(), JobStoreError> {
        let mut state = self.locked();
        let job = state.claimed_mut(lease)?;
        job.state = JobState::Succeeded;
        job.lease = None;
        job.claimed_by = None;
        job.claimed_at = None;
        job.last_heartbeat_at = None;
        Ok(())
    }

    async fn fail(&self, request: FailRequest) -> Result<FailOutcome, JobStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();

        // The run's policy is needed before the job is mutably borrowed.
        let run_id = state
            .jobs
            .get(&request.lease.job_id)
            .ok_or(JobStoreError::UnknownJob {
                job_id: request.lease.job_id,
            })?
            .run_id;
        let run = state
            .runs
            .get(&run_id)
            .ok_or(JobStoreError::UnknownRun { run_id })?;
        let max_attempts = run.max_attempts;
        let backoff = run.backoff;

        let job = state.claimed_mut(request.lease)?;
        job.lease = None;
        job.claimed_by = None;
        job.claimed_at = None;
        job.last_heartbeat_at = None;
        job.last_failure = Some(request.reason);

        if job.attempts >= max_attempts {
            job.state = JobState::Abandoned;
            return Ok(FailOutcome::Abandoned);
        }

        let step = chrono::Duration::from_std(backoff).unwrap_or(chrono::Duration::zero());
        let claimable_at = now + step;
        job.state = JobState::Pending;
        job.claimable_at = claimable_at;

        Ok(FailOutcome::Requeued {
            claimable_at,
            attempts_remaining: max_attempts - job.attempts,
        })
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-jobs memory`
Expected: PASS — the six new tests and the two backoff tests from Task 5 all green. Report the total.

- [ ] **Step 5: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add oxo-jobs/
git commit -F - <<'EOF'
feat(jobs): report on a held job, and retire one that cannot succeed

heartbeat, complete and fail all resolve the caller's lease through one
shared check, so the three ways a lease can be invalid cannot drift apart
between them. A stale token is LeaseLost rather than being ignored: a
worker whose job was reclaimed must stop, not overwrite the state of
whoever holds it now.

fail requeues with the run's snapshotted backoff while starts remain and
abandons on the last permitted one. Because attempts increment at claim,
the budget is spent by starting, so abandonment lands exactly at
max_attempts starts however the job got there.

Also turns green the two backoff tests Task 5 left failing.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 7: In-memory adapter — `reap_expired`, `run_status`, `throughput`

**Files:**
- Modify: `oxo-jobs/src/memory.rs`
- Modify: `oxo-jobs/src/error.rs`

**Interfaces:**
- Produces: the three methods replacing their stubs, plus `JobStoreError::EmptyRun`.

**A late addition, caught while designing `run_status`.** A run with no jobs would report `Complete`, because "every job succeeded" is vacuously true of none — a silently wrong answer of exactly the kind this project keeps finding. The specification layer already guarantees a non-empty tile set, so this is defence in depth, but a store that cheerfully declares nothing finished is a trap. `create_run` must reject an empty job set with a new `JobStoreError::EmptyRun` variant. Add the variant and extend Task 2's `every_variant_renders_something_an_operator_can_act_on` table with a row for it.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `oxo-jobs/src/memory.rs`:

```rust
    fn reap(heartbeat_secs: u64, max_secs: u64) -> ReapRequest {
        ReapRequest {
            heartbeat_timeout: Duration::from_secs(heartbeat_secs),
            max_job_duration: Duration::from_secs(max_secs),
        }
    }

    #[tokio::test]
    async fn a_job_whose_heartbeat_lapses_is_reclaimed_and_takes_backoff() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_run(two_tile_run()).await.unwrap();
        let job = claim_one(&store).await;

        // Within the timeout: nothing is reclaimed.
        test_clock.advance(Duration::from_secs(60));
        let quiet = store.reap_expired(reap(90, 86_400)).await.expect("reap");
        assert_eq!(quiet, ReapOutcome::default());
        store.heartbeat(lease_of(&job)).await.expect("still held");

        // Past the timeout with no further heartbeat: reclaimed.
        test_clock.advance(Duration::from_secs(91));
        let reaped = store.reap_expired(reap(90, 86_400)).await.expect("reap");
        assert_eq!(reaped.requeued, 1);
        assert_eq!(reaped.abandoned, 0);

        // The old lease is now worthless.
        let error = store.heartbeat(lease_of(&job)).await.expect_err("reclaimed");
        assert!(
            matches!(error, JobStoreError::LeaseLost { .. } | JobStoreError::NotClaimed { .. }),
            "expected the old lease to be refused, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_reaped_job_is_not_instantly_reclaimable() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_run(two_tile_run()).await.unwrap();
        claim_one(&store).await;
        store.claim(any("pod-2")).await.unwrap().unwrap();

        test_clock.advance(Duration::from_secs(100));
        store.reap_expired(reap(90, 86_400)).await.expect("reap");

        assert!(
            store.claim(any("pod-3")).await.unwrap().is_none(),
            "a reaped job takes backoff; re-claiming it instantly would burn its budget in minutes"
        );
        test_clock.advance(Duration::from_secs(60));
        assert!(store.claim(any("pod-3")).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_worker_that_heartbeats_forever_is_still_cut_off_by_the_backstop() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_run(two_tile_run()).await.unwrap();
        let job = claim_one(&store).await;

        // Heartbeat diligently for well past the maximum duration.
        for _ in 0..10 {
            test_clock.advance(Duration::from_secs(60));
            let _ = store.heartbeat(lease_of(&job)).await;
        }

        let reaped = store.reap_expired(reap(90, 300)).await.expect("reap");
        assert_eq!(
            reaped.requeued, 1,
            "a wedged-but-alive worker must not hold a job indefinitely"
        );
    }

    #[tokio::test]
    async fn a_reap_consumes_an_attempt_and_can_abandon() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        let mut run = two_tile_run();
        run.max_attempts = 1;
        store.create_run(run).await.unwrap();

        claim_one(&store).await;
        test_clock.advance(Duration::from_secs(100));

        let reaped = store.reap_expired(reap(90, 86_400)).await.expect("reap");
        assert_eq!(
            (reaped.requeued, reaped.abandoned),
            (0, 1),
            "the single permitted start was spent by claiming, so the reap abandons"
        );
    }

    #[tokio::test]
    async fn the_gate_reports_in_progress_then_complete() {
        let store = store(clock());
        let run = store.create_run(two_tile_run()).await.unwrap();

        assert_eq!(
            store.run_status(run.run_id).await.unwrap(),
            RunStatus::InProgress {
                pending: 2,
                claimed: 0,
                succeeded: 0,
                abandoned: 0
            }
        );

        for _ in 0..2 {
            let job = claim_one(&store).await;
            store.complete(lease_of(&job)).await.unwrap();
        }

        assert_eq!(store.run_status(run.run_id).await.unwrap(), RunStatus::Complete);
    }

    #[tokio::test]
    async fn an_abandoned_job_is_visible_while_work_continues_then_fails_the_run() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        let mut spec = two_tile_run();
        spec.max_attempts = 1;
        let run = store.create_run(spec).await.unwrap();

        let doomed = claim_one(&store).await;
        store
            .fail(FailRequest {
                lease: lease_of(&doomed),
                reason: "Crash!".to_string(),
            })
            .await
            .unwrap();

        // One tile is unrecoverable, but the other is still runnable — and
        // the operator can see the problem now rather than in a fortnight.
        assert_eq!(
            store.run_status(run.run_id).await.unwrap(),
            RunStatus::InProgress {
                pending: 1,
                claimed: 0,
                succeeded: 0,
                abandoned: 1
            }
        );

        let other = claim_one(&store).await;
        store.complete(lease_of(&other)).await.unwrap();

        assert_eq!(
            store.run_status(run.run_id).await.unwrap(),
            RunStatus::Failed { abandoned: 1 }
        );
    }

    #[tokio::test]
    async fn the_gate_rejects_a_run_it_does_not_know() {
        let store = store(clock());
        let error = store
            .run_status(RunId::generate())
            .await
            .expect_err("unknown run");
        assert!(
            matches!(error, JobStoreError::UnknownRun { .. }),
            "expected UnknownRun, got {error:?}"
        );
    }

    #[tokio::test]
    async fn throughput_separates_pending_from_claimable_now() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        let run = store.create_run(two_tile_run()).await.unwrap();

        let job = claim_one(&store).await;
        store
            .fail(FailRequest {
                lease: lease_of(&job),
                reason: "Crash!".to_string(),
            })
            .await
            .unwrap();

        let snapshot = store.throughput(run.run_id).await.unwrap();
        assert_eq!(snapshot.pending, 2, "both jobs are pending");
        assert_eq!(
            snapshot.claimable_now, 1,
            "one is in backoff, so only one can be claimed right now"
        );

        test_clock.advance(Duration::from_secs(60));
        let later = store.throughput(run.run_id).await.unwrap();
        assert_eq!(later.claimable_now, 2);
    }

    #[tokio::test]
    async fn a_run_with_no_jobs_is_refused_rather_than_declared_complete() {
        let store = store(clock());
        let mut empty = two_tile_run();
        empty.jobs.clear();
        let error = store.create_run(empty).await.expect_err("empty run");
        assert!(
            matches!(error, JobStoreError::EmptyRun { .. }),
            "expected EmptyRun, got {error:?}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-jobs memory`
Expected: FAIL — the new tests panic at `unimplemented!("Task 7")`, and the empty-run test fails to compile because `JobStoreError::EmptyRun` does not exist.

- [ ] **Step 3: Add the `EmptyRun` variant**

Add to `JobStoreError` in `oxo-jobs/src/error.rs`:

```rust
    /// A run must contain at least one job. A run of none would report
    /// `Complete` vacuously, which is a silently wrong answer.
    #[error("a run for region {region_code} revision {revision} must contain at least one job")]
    EmptyRun { region_code: String, revision: u32 },
```

Extend the `every_variant_renders_something_an_operator_can_act_on` table in that file's tests with:

```rust
            (
                JobStoreError::EmptyRun {
                    region_code: "NA".to_string(),
                    revision: 1,
                },
                "at least one job",
            ),
```

Then add the guard as the first thing `create_run` does, before the identity lookup:

```rust
        if request.jobs.is_empty() {
            return Err(JobStoreError::EmptyRun {
                region_code: request.region_code,
                revision: request.revision,
            });
        }
```

- [ ] **Step 4: Implement the three methods**

Add a helper to the `impl State` block:

```rust
impl State {
    /// Count jobs per state for one run. Shared by the gate and the
    /// throughput snapshot so the two cannot disagree.
    fn tally(&self, run_id: RunId, now: DateTime<Utc>) -> Result<Tally, JobStoreError> {
        let run = self
            .runs
            .get(&run_id)
            .ok_or(JobStoreError::UnknownRun { run_id })?;
        let mut tally = Tally::default();
        for id in &run.job_ids {
            let job = &self.jobs[id];
            match job.state {
                JobState::Pending => {
                    tally.pending += 1;
                    if job.claimable_at <= now {
                        tally.claimable_now += 1;
                    }
                }
                JobState::Claimed => tally.claimed += 1,
                JobState::Succeeded => tally.succeeded += 1,
                JobState::Abandoned => tally.abandoned += 1,
            }
        }
        Ok(tally)
    }
}

#[derive(Debug, Default)]
struct Tally {
    pending: u32,
    claimable_now: u32,
    claimed: u32,
    succeeded: u32,
    abandoned: u32,
}
```

Then replace the three stubs:

```rust
    async fn reap_expired(&self, request: ReapRequest) -> Result<ReapOutcome, JobStoreError> {
        let now = self.clock.now();
        let heartbeat_timeout = chrono::Duration::from_std(request.heartbeat_timeout)
            .unwrap_or(chrono::Duration::zero());
        let max_duration = chrono::Duration::from_std(request.max_job_duration)
            .unwrap_or(chrono::Duration::zero());
        let mut state = self.locked();

        // Policies are read before any job is mutably borrowed.
        let policies: BTreeMap<RunId, (u32, Duration)> = state
            .runs
            .iter()
            .map(|(id, run)| (*id, (run.max_attempts, run.backoff)))
            .collect();

        let expired: Vec<JobId> = state
            .jobs
            .iter()
            .filter(|(_, job)| job.state == JobState::Claimed)
            .filter(|(_, job)| {
                let heartbeat_lapsed = job
                    .last_heartbeat_at
                    .is_some_and(|last| now - last > heartbeat_timeout);
                let held_too_long = job
                    .claimed_at
                    .is_some_and(|since| now - since > max_duration);
                heartbeat_lapsed || held_too_long
            })
            .map(|(id, _)| *id)
            .collect();

        let mut outcome = ReapOutcome::default();
        for job_id in expired {
            let (max_attempts, backoff) = policies[&state.jobs[&job_id].run_id];
            let job = state.jobs.get_mut(&job_id).expect("just selected");
            job.lease = None;
            job.claimed_by = None;
            job.claimed_at = None;
            job.last_heartbeat_at = None;

            if job.attempts >= max_attempts {
                job.state = JobState::Abandoned;
                outcome.abandoned += 1;
            } else {
                let step =
                    chrono::Duration::from_std(backoff).unwrap_or(chrono::Duration::zero());
                job.state = JobState::Pending;
                job.claimable_at = now + step;
                outcome.requeued += 1;
            }
        }

        Ok(outcome)
    }

    async fn run_status(&self, run_id: RunId) -> Result<RunStatus, JobStoreError> {
        let now = self.clock.now();
        let state = self.locked();
        let tally = state.tally(run_id, now)?;

        if tally.pending == 0 && tally.claimed == 0 {
            return Ok(if tally.abandoned == 0 {
                RunStatus::Complete
            } else {
                RunStatus::Failed {
                    abandoned: tally.abandoned,
                }
            });
        }

        Ok(RunStatus::InProgress {
            pending: tally.pending,
            claimed: tally.claimed,
            succeeded: tally.succeeded,
            abandoned: tally.abandoned,
        })
    }

    async fn throughput(&self, run_id: RunId) -> Result<Throughput, JobStoreError> {
        let now = self.clock.now();
        let state = self.locked();
        let tally = state.tally(run_id, now)?;
        Ok(Throughput {
            pending: tally.pending,
            claimable_now: tally.claimable_now,
            claimed: tally.claimed,
            succeeded: tally.succeeded,
            abandoned: tally.abandoned,
        })
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --package oxo-jobs`
Expected: PASS. Every `unimplemented!` is now gone — confirm with
`grep -c 'unimplemented!' oxo-jobs/src/memory.rs`, which must print `0`.

- [ ] **Step 6: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add oxo-jobs/
git commit -F - <<'EOF'
feat(jobs): reclaim dead work, and answer whether a region is finished

A reap reclaims a claimed job on either of two conditions: its heartbeat
lapsed, or it has been held past the maximum duration regardless of
heartbeats — the latter covering a worker that is wedged but alive and
would otherwise hold a job forever. Either way it takes the same backoff
as a reported failure, so a tile that kills its worker cannot be
re-claimed instantly and spend its whole budget in minutes. Because
attempts increment at claim, a reap with the budget already spent
abandons rather than requeueing, which is what stops such a tile cycling.

The gate and the throughput snapshot share one tally, so they cannot
disagree. Abandoned jobs are reported during InProgress as well as in
Failed: a run with an abandoned tile is already unachievable, and an
operator should learn that in minutes rather than after a fortnight of
other tiles finishing.

Also refuses a run with no jobs. Planning found that one would report
Complete vacuously, which is a silently wrong answer of exactly the kind
this project keeps uncovering.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 8: The conformance suite

**Files:**
- Create: `oxo-jobs/src/conformance.rs`
- Create: `oxo-jobs/tests/conformance_memory.rs`
- Modify: `oxo-jobs/src/lib.rs`
- Modify: `oxo-jobs/Cargo.toml`

**Interfaces:**
- Produces: a `conformance` feature; `Subject { store, clock }`; the `Fixture` trait with `async fn fresh(&self) -> Subject`; one `pub async fn` per invariant; and the `conformance_suite!` macro, which expands to one `#[tokio::test]` per case.
- Consumed by: `oxo-jobs/tests/conformance_memory.rs` now, and `oxo-jobs-postgres` in Task 12.

**Why a macro.** Both adapters must run every case, and adding a case later must not mean editing two crates. The macro keeps the case list in exactly one place — here — while giving each case its own test name in the output, so a failure names the invariant that broke rather than "the suite".

- [ ] **Step 1: Add the feature**

Modify `oxo-jobs/Cargo.toml`:

```toml
[features]
# Exposes the conformance suite so another crate's adapter can be held to
# the same contract. Off by default: it is test scaffolding, not API.
conformance = []

[dev-dependencies]
tokio = { workspace = true }
```

and add, so the crate's own conformance test target can see the module:

```toml
[[test]]
name = "conformance_memory"
required-features = ["conformance"]
```

- [ ] **Step 2: Write the suite**

Create `oxo-jobs/src/conformance.rs`:

```rust
//! A contract every [`JobStore`] adapter must satisfy.
//!
//! Behind the `conformance` feature, because this is test scaffolding
//! rather than API. The same cases run against the in-memory adapter and
//! the PostgreSQL one, which is what stops the in-memory adapter drifting
//! into a convenient fiction that passes while the real store would not.
//!
//! Cases assert invariants, never implementation. Use
//! [`conformance_suite!`] to generate one test per case.
//!
//! [`conformance_suite!`]: crate::conformance_suite

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use oxo_spec::TileId;

use crate::clock::TestClock;
use crate::error::JobStoreError;
use crate::job::JobType;
use crate::request::{
    ClaimRequest, ClaimedJob, CreateRun, FailOutcome, FailRequest, JobSpec, Lease, ReapRequest,
    RunStatus,
};
use crate::store::JobStore;

/// A freshly-made, empty store and the clock it reads.
pub struct Subject {
    pub store: Box<dyn JobStore>,
    pub clock: Arc<TestClock>,
}

/// Produces a fresh, empty [`Subject`] for each case.
///
/// An adapter backed by a database must give each case genuine isolation —
/// its own schema, or a truncated one — or cases will interfere and the
/// failures will be baffling.
#[async_trait]
pub trait Fixture: Send + Sync {
    async fn fresh(&self) -> Subject;
}

fn tile(lat: i8, lon: i16) -> TileId {
    TileId::new(lat, lon).expect("in range")
}

/// Two jobs for one tile: the ortho build and its overlay.
pub fn two_job_run() -> CreateRun {
    CreateRun {
        region_code: "NA".to_string(),
        revision: 1,
        max_attempts: 3,
        backoff: Duration::from_secs(60),
        jobs: vec![
            JobSpec {
                tile: tile(50, -2),
                job_type: JobType::Ortho,
            },
            JobSpec {
                tile: tile(50, -2),
                job_type: JobType::Overlay,
            },
        ],
    }
}

fn any_job(worker: &str) -> ClaimRequest {
    ClaimRequest {
        worker: worker.to_string(),
        job_types: None,
    }
}

fn lease_of(job: &ClaimedJob) -> Lease {
    Lease {
        job_id: job.job_id,
        token: job.lease,
    }
}

// ─── cases ───────────────────────────────────────────────────────────────

pub async fn creating_a_run_twice_resumes_it(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let first = subject.store.create_run(two_job_run()).await.expect("create");
    let second = subject.store.create_run(two_job_run()).await.expect("resume");
    assert!(first.created);
    assert!(!second.created);
    assert_eq!(first.run_id, second.run_id);
    assert_eq!(second.total_jobs, 2);
}

pub async fn a_changed_job_set_under_one_identity_conflicts(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject.store.create_run(two_job_run()).await.expect("create");
    let mut altered = two_job_run();
    altered.jobs.push(JobSpec {
        tile: tile(51, -2),
        job_type: JobType::Ortho,
    });
    let error = subject
        .store
        .create_run(altered)
        .await
        .expect_err("should conflict");
    assert!(
        matches!(error, JobStoreError::RunConflict { .. }),
        "expected RunConflict, got {error:?}"
    );
}

pub async fn a_run_with_no_jobs_is_refused(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut empty = two_job_run();
    empty.jobs.clear();
    let error = subject
        .store
        .create_run(empty)
        .await
        .expect_err("should refuse");
    assert!(
        matches!(error, JobStoreError::EmptyRun { .. }),
        "expected EmptyRun, got {error:?}"
    );
}

pub async fn every_job_is_handed_out_exactly_once(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject.store.create_run(two_job_run()).await.expect("create");

    let mut seen = Vec::new();
    while let Some(job) = subject
        .store
        .claim(any_job("pod"))
        .await
        .expect("claim")
    {
        seen.push(job.job_id);
    }
    seen.sort();
    let unique = {
        let mut copy = seen.clone();
        copy.dedup();
        copy
    };
    assert_eq!(seen.len(), 2, "both jobs were handed out");
    assert_eq!(seen, unique, "no job was handed out twice");
}

pub async fn an_empty_queue_yields_none_not_an_error(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    assert!(subject
        .store
        .claim(any_job("pod"))
        .await
        .expect("claim")
        .is_none());
}

pub async fn a_job_type_filter_is_honoured(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject.store.create_run(two_job_run()).await.expect("create");
    let claimed = subject
        .store
        .claim(ClaimRequest {
            worker: "pod".to_string(),
            job_types: Some(vec![JobType::Overlay]),
        })
        .await
        .expect("claim")
        .expect("an overlay job exists");
    assert_eq!(claimed.job_type, JobType::Overlay);
}

pub async fn a_stale_lease_is_refused_by_every_reporting_call(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject.store.create_run(two_job_run()).await.expect("create");
    let job = subject
        .store
        .claim(any_job("pod"))
        .await
        .expect("claim")
        .expect("a job");
    let stale = Lease {
        job_id: job.job_id,
        token: crate::ids::LeaseToken::generate(),
    };

    for error in [
        subject.store.heartbeat(stale).await.expect_err("heartbeat"),
        subject.store.complete(stale).await.expect_err("complete"),
        subject
            .store
            .fail(FailRequest {
                lease: stale,
                reason: "Crash!".to_string(),
            })
            .await
            .expect_err("fail"),
    ] {
        assert!(
            matches!(error, JobStoreError::LeaseLost { .. }),
            "expected LeaseLost, got {error:?}"
        );
    }
}

pub async fn a_failure_is_requeued_until_the_budget_is_spent(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut run = two_job_run();
    run.max_attempts = 2;
    subject.store.create_run(run).await.expect("create");

    let first = subject
        .store
        .claim(any_job("pod"))
        .await
        .unwrap()
        .expect("a job");
    let requeued = subject
        .store
        .fail(FailRequest {
            lease: lease_of(&first),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");
    assert!(
        matches!(requeued, FailOutcome::Requeued { attempts_remaining: 1, .. }),
        "expected one start remaining, got {requeued:?}"
    );

    subject.clock.advance(Duration::from_secs(60));
    let retry = loop {
        if let Some(job) = subject.store.claim(any_job("pod")).await.unwrap() {
            if job.job_id == first.job_id {
                break job;
            }
        } else {
            panic!("the requeued job never became claimable");
        }
    };
    assert_eq!(retry.attempt, 2);

    let abandoned = subject
        .store
        .fail(FailRequest {
            lease: lease_of(&retry),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");
    assert_eq!(abandoned, FailOutcome::Abandoned);
}

pub async fn a_lapsed_heartbeat_reclaims_the_job_and_spends_a_start(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut run = two_job_run();
    run.max_attempts = 1;
    subject.store.create_run(run).await.expect("create");

    subject.store.claim(any_job("pod")).await.unwrap().expect("a job");
    subject.clock.advance(Duration::from_secs(100));

    let reaped = subject
        .store
        .reap_expired(ReapRequest {
            heartbeat_timeout: Duration::from_secs(90),
            max_job_duration: Duration::from_secs(86_400),
        })
        .await
        .expect("reap");
    assert_eq!(
        (reaped.requeued, reaped.abandoned),
        (0, 1),
        "the one permitted start was spent by claiming, so the reap abandons"
    );
}

pub async fn a_diligent_but_wedged_worker_is_cut_off_by_the_backstop(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject.store.create_run(two_job_run()).await.expect("create");
    let job = subject
        .store
        .claim(any_job("pod"))
        .await
        .unwrap()
        .expect("a job");

    for _ in 0..10 {
        subject.clock.advance(Duration::from_secs(30));
        let _ = subject.store.heartbeat(lease_of(&job)).await;
    }

    let reaped = subject
        .store
        .reap_expired(ReapRequest {
            heartbeat_timeout: Duration::from_secs(90),
            max_job_duration: Duration::from_secs(120),
        })
        .await
        .expect("reap");
    assert_eq!(reaped.requeued, 1);
}

pub async fn the_gate_moves_from_in_progress_to_complete(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let run = subject.store.create_run(two_job_run()).await.expect("create");
    assert!(matches!(
        subject.store.run_status(run.run_id).await.expect("status"),
        RunStatus::InProgress { pending: 2, .. }
    ));

    while let Some(job) = subject.store.claim(any_job("pod")).await.unwrap() {
        subject.store.complete(lease_of(&job)).await.expect("complete");
    }

    assert_eq!(
        subject.store.run_status(run.run_id).await.expect("status"),
        RunStatus::Complete
    );
}

pub async fn an_abandoned_job_is_visible_before_it_fails_the_run(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut spec = two_job_run();
    spec.max_attempts = 1;
    let run = subject.store.create_run(spec).await.expect("create");

    let doomed = subject
        .store
        .claim(any_job("pod"))
        .await
        .unwrap()
        .expect("a job");
    subject
        .store
        .fail(FailRequest {
            lease: lease_of(&doomed),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");

    assert!(
        matches!(
            subject.store.run_status(run.run_id).await.expect("status"),
            RunStatus::InProgress { abandoned: 1, .. }
        ),
        "an unachievable run must be visible while other work continues"
    );

    let other = subject
        .store
        .claim(any_job("pod"))
        .await
        .unwrap()
        .expect("a job");
    subject.store.complete(lease_of(&other)).await.expect("complete");

    assert_eq!(
        subject.store.run_status(run.run_id).await.expect("status"),
        RunStatus::Failed { abandoned: 1 }
    );
}

pub async fn an_unknown_run_is_refused_by_the_gate(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let error = subject
        .store
        .run_status(crate::ids::RunId::generate())
        .await
        .expect_err("unknown run");
    assert!(
        matches!(error, JobStoreError::UnknownRun { .. }),
        "expected UnknownRun, got {error:?}"
    );
}

pub async fn throughput_separates_pending_from_claimable_now(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let run = subject.store.create_run(two_job_run()).await.expect("create");
    let job = subject
        .store
        .claim(any_job("pod"))
        .await
        .unwrap()
        .expect("a job");
    subject
        .store
        .fail(FailRequest {
            lease: lease_of(&job),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");

    let snapshot = subject.store.throughput(run.run_id).await.expect("throughput");
    assert_eq!(snapshot.pending, 2);
    assert_eq!(snapshot.claimable_now, 1);

    subject.clock.advance(Duration::from_secs(60));
    let later = subject.store.throughput(run.run_id).await.expect("throughput");
    assert_eq!(later.claimable_now, 2);
}

/// The invariant that matters most under a real database: concurrent
/// claimants must between them see each job exactly once.
pub async fn concurrent_claims_hand_each_job_out_exactly_once(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut run = two_job_run();
    run.jobs = (0..24)
        .map(|n| JobSpec {
            tile: tile(50, -24 + n),
            job_type: JobType::Ortho,
        })
        .collect();
    let created = subject.store.create_run(run).await.expect("create");
    assert_eq!(created.total_jobs, 24);

    let store: Arc<dyn JobStore> = Arc::from(subject.store);
    let mut workers = Vec::new();
    for worker in 0..8 {
        let store = Arc::clone(&store);
        workers.push(tokio::spawn(async move {
            let mut mine = Vec::new();
            while let Some(job) = store
                .claim(ClaimRequest {
                    worker: format!("pod-{worker}"),
                    job_types: None,
                })
                .await
                .expect("claim")
            {
                mine.push(job.job_id);
            }
            mine
        }));
    }

    let mut all = Vec::new();
    for worker in workers {
        all.extend(worker.await.expect("worker did not panic"));
    }
    all.sort();
    let mut unique = all.clone();
    unique.dedup();

    assert_eq!(all.len(), 24, "every job was claimed");
    assert_eq!(all, unique, "no job was claimed twice");
}
```

- [ ] **Step 3: Write the macro**

Append to `oxo-jobs/src/conformance.rs`:

```rust
/// Generate one `#[tokio::test]` per conformance case.
///
/// Takes an expression producing a [`Fixture`]. The case list lives here and
/// nowhere else, so adding a case covers every adapter without touching
/// their crates.
#[macro_export]
macro_rules! conformance_suite {
    ($fixture:expr) => {
        $crate::conformance_case!($fixture, creating_a_run_twice_resumes_it);
        $crate::conformance_case!($fixture, a_changed_job_set_under_one_identity_conflicts);
        $crate::conformance_case!($fixture, a_run_with_no_jobs_is_refused);
        $crate::conformance_case!($fixture, every_job_is_handed_out_exactly_once);
        $crate::conformance_case!($fixture, an_empty_queue_yields_none_not_an_error);
        $crate::conformance_case!($fixture, a_job_type_filter_is_honoured);
        $crate::conformance_case!($fixture, a_stale_lease_is_refused_by_every_reporting_call);
        $crate::conformance_case!($fixture, a_failure_is_requeued_until_the_budget_is_spent);
        $crate::conformance_case!($fixture, a_lapsed_heartbeat_reclaims_the_job_and_spends_a_start);
        $crate::conformance_case!($fixture, a_diligent_but_wedged_worker_is_cut_off_by_the_backstop);
        $crate::conformance_case!($fixture, the_gate_moves_from_in_progress_to_complete);
        $crate::conformance_case!($fixture, an_abandoned_job_is_visible_before_it_fails_the_run);
        $crate::conformance_case!($fixture, an_unknown_run_is_refused_by_the_gate);
        $crate::conformance_case!($fixture, throughput_separates_pending_from_claimable_now);
        $crate::conformance_case!($fixture, concurrent_claims_hand_each_job_out_exactly_once);
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! conformance_case {
    ($fixture:expr, $case:ident) => {
        #[tokio::test]
        async fn $case() {
            let fixture = $fixture;
            $crate::conformance::$case(&fixture).await;
        }
    };
}
```

- [ ] **Step 4: Wire the module and write the in-memory fixture**

Add to `oxo-jobs/src/lib.rs`:

```rust
#[cfg(feature = "conformance")]
pub mod conformance;
```

Create `oxo-jobs/tests/conformance_memory.rs`:

```rust
//! The conformance suite, run against the in-memory adapter.
//!
//! The same cases run against `oxo-jobs-postgres`. If one adapter passes a
//! case the other fails, the two have diverged — which is the whole reason
//! this suite exists.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use oxo_jobs::clock::TestClock;
use oxo_jobs::conformance::{Fixture, Subject};
use oxo_jobs::InMemoryJobStore;

struct Memory;

#[async_trait]
impl Fixture for Memory {
    async fn fresh(&self) -> Subject {
        let clock = Arc::new(TestClock::new(
            chrono::Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        ));
        Subject {
            store: Box::new(InMemoryJobStore::new(clock.clone())),
            clock,
        }
    }
}

oxo_jobs::conformance_suite!(Memory);
```

- [ ] **Step 5: Run the suite**

Run: `cargo test --package oxo-jobs --features conformance --test conformance_memory`
Expected: PASS, 15 tests — one per case, each named after the invariant it asserts.

- [ ] **Step 6: Confirm the suite is reachable from `make verify`**

`make verify` runs `--all-targets`, which does not enable optional features. Modify the `test` and `test-strict` targets in the `Makefile` to add `--all-features` so the conformance suite is covered by the default verification:

```makefile
.PHONY: test
test: ## Run all tests
	$(CARGO) test --workspace --all-targets --all-features

.PHONY: test-strict
test-strict: ## Run all tests with warnings as errors (matches CI)
	RUSTFLAGS="-D warnings" $(CARGO) test --workspace --all-targets --all-features
```

Run: `make verify`
Expected: PASS, and the output must include the 15 conformance tests. **Confirm that by reading the test counts** — if `make verify` passes without running them, the feature is not being enabled and the suite is invisible, which is the failure this step exists to prevent.

- [ ] **Step 7: Commit**

```bash
git add oxo-jobs/ Makefile
git commit -F - <<'EOF'
test(jobs): add the conformance suite both adapters must satisfy

Fifteen cases asserting invariants rather than implementation: a job is
handed out exactly once, a stale lease is refused by every reporting
call, a reap spends a start, a wedged worker is cut off by the backstop,
the gate moves through its three shapes, and concurrent claimants between
them see each job exactly once.

The case list lives in one macro, so adding a case covers every adapter
without touching their crates, while each case still gets its own test
name — a failure names the invariant that broke rather than "the suite".

make verify now passes --all-features so the suite is part of default
verification rather than something only a separate command reaches.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 9: `oxo-jobs-postgres` — crate, schema, and the disposable database

**Files:**
- Modify: `Cargo.toml`
- Modify: `Makefile`
- Create: `oxo-jobs-postgres/Cargo.toml`
- Create: `oxo-jobs-postgres/migrations/0001_jobs.sql`
- Create: `oxo-jobs-postgres/src/lib.rs`

**Interfaces:**
- Produces: `PostgresJobStore::new(pool: PgPool, clock: Arc<dyn Clock>)`, `run_migrations(&PgPool) -> Result<(), sqlx::Error>`, and the schema. Port methods are stubbed with `unimplemented!()` naming the task that fills them (10, 11, 12).
- Make targets: `pg-up`, `pg-down`, `verify-db`.

**Job type and state are stored as `text` with a `CHECK` constraint, not as PostgreSQL enums.** A PostgreSQL enum would need `#[derive(sqlx::Type)]` on `JobType`, which would drag `sqlx` into `oxo-jobs` and break the dependency boundary that is the whole point of the split. `text` plus `CHECK` keeps the constraint in the database while the conversion uses the exact string forms Task 2 built for precisely this.

- [ ] **Step 1: Add the crate and the test exclusion**

Add `"oxo-jobs-postgres"` to `[workspace] members` and add to `[workspace.dependencies]`:

```toml
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "tls-none", "postgres", "uuid", "chrono", "macros", "migrate"] }
```

Create `oxo-jobs-postgres/Cargo.toml`:

```toml
[package]
name = "oxo-jobs-postgres"
description = "PostgreSQL adapter for the OXO job store"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
async-trait = { workspace = true }
chrono = { workspace = true }
oxo-jobs = { path = "../oxo-jobs" }
oxo-spec = { path = "../oxo-spec" }
sqlx = { workspace = true }
uuid = { workspace = true }

[dev-dependencies]
oxo-jobs = { path = "../oxo-jobs", features = ["conformance"] }
tokio = { workspace = true }

[[test]]
name = "conformance_postgres"
```

Modify the `test` and `test-strict` targets in the `Makefile` to exclude this package by name:

```makefile
.PHONY: test
test: ## Run all tests except those needing a database (see verify-db)
	$(CARGO) test --workspace --exclude oxo-jobs-postgres --all-targets --all-features

.PHONY: test-strict
test-strict: ## Run all tests except database ones, warnings as errors
	RUSTFLAGS="-D warnings" $(CARGO) test --workspace --exclude oxo-jobs-postgres --all-targets --all-features
```

**Excluded by name, never skipped at runtime.** A runtime skip is how a suite reports green while testing nothing — the trap sub-project 1 hit, where a renamed Gherkin step became a silent skip at exit code 0. Exclusion by name means the absence of database coverage is visible in which target you ran. `make lint` keeps `--workspace` with no exclusion, so this crate is still compiled and linted by default verification.

- [ ] **Step 2: Add the disposable-database targets**

Append to the `Makefile`:

```makefile
PG_TEST_CONTAINER ?= oxo-jobs-test-pg
PG_TEST_PORT ?= 55432
PG_TEST_URL ?= postgres://postgres:postgres@127.0.0.1:$(PG_TEST_PORT)/postgres

.PHONY: pg-up
pg-up: ## Start a disposable PostgreSQL for the adapter tests
	podman run --rm -d --name $(PG_TEST_CONTAINER) -e POSTGRES_PASSWORD=postgres -p $(PG_TEST_PORT):5432 docker.io/library/postgres:17-alpine
	printf 'waiting for postgres'
	for i in $$(seq 1 60); do \
	  if podman exec $(PG_TEST_CONTAINER) pg_isready -q -U postgres 2>/dev/null; then echo ' ready'; exit 0; fi; \
	  printf '.'; sleep 1; \
	done; echo ' timed out'; exit 1

.PHONY: pg-down
pg-down: ## Remove the disposable PostgreSQL
	-podman rm -f $(PG_TEST_CONTAINER) >/dev/null 2>&1 || true

.PHONY: verify-db
verify-db: ## Run the conformance suite against a real PostgreSQL
	$(MAKE) pg-up
	DATABASE_URL=$(PG_TEST_URL) $(CARGO) test --package oxo-jobs-postgres --all-features; \
	status=$$?; $(MAKE) pg-down; exit $$status
```

The teardown runs whether the tests passed or failed, so a failure does not
leave a container behind. If `podman` is unavailable the target fails
loudly, which is correct — it cannot do its job.

- [ ] **Step 3: Write the schema**

Create `oxo-jobs-postgres/migrations/0001_jobs.sql`:

```sql
-- Job type and state are text with a CHECK rather than PostgreSQL enums.
-- An enum would require sqlx::Type on the Rust enums, dragging sqlx into
-- oxo-jobs and breaking the dependency boundary the crate split exists to
-- maintain. The accepted labels match JobType::as_str and JobState::as_str
-- exactly, and from_str_exact does no case folding, so the two cannot drift
-- apart silently.

CREATE TABLE runs (
    id           uuid        PRIMARY KEY,
    region_code  text        NOT NULL,
    revision     integer     NOT NULL,
    -- Snapshotted from the specification's failure policy, so editing a
    -- specification cannot change the policy of a run already in flight.
    max_attempts integer     NOT NULL CHECK (max_attempts >= 1),
    backoff_secs bigint      NOT NULL CHECK (backoff_secs >= 0),
    created_at   timestamptz NOT NULL,
    UNIQUE (region_code, revision)
);

CREATE TABLE jobs (
    id                uuid        PRIMARY KEY,
    run_id            uuid        NOT NULL REFERENCES runs (id) ON DELETE CASCADE,
    tile              text        NOT NULL,
    job_type          text        NOT NULL CHECK (job_type IN ('ortho', 'overlay')),
    state             text        NOT NULL CHECK (state IN ('pending', 'claimed', 'succeeded', 'abandoned')),
    -- Counts starts, not failures: incremented at claim.
    attempts          integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claimable_at      timestamptz NOT NULL,
    lease_token       uuid,
    claimed_by        text,
    claimed_at        timestamptz,
    last_heartbeat_at timestamptz,
    last_failure      text,
    UNIQUE (run_id, tile, job_type),
    -- A claimed job holds a lease and a claim time; nothing else does.
    CONSTRAINT lease_matches_state CHECK (
        (state = 'claimed') = (lease_token IS NOT NULL)
        AND (state = 'claimed') = (claimed_at IS NOT NULL)
    )
);

-- The claim query's hot path.
CREATE INDEX jobs_claimable ON jobs (claimable_at, id) WHERE state = 'pending';
-- The reaper's hot path.
CREATE INDEX jobs_claimed ON jobs (last_heartbeat_at) WHERE state = 'claimed';
```

The `lease_matches_state` constraint is deliberate: it makes "a job holds a lease if and only if it is claimed" a property the database enforces, so an adapter bug that leaves a dangling lease is a write failure rather than silent corruption.

- [ ] **Step 4: Write the crate skeleton**

Create `oxo-jobs-postgres/src/lib.rs`:

```rust
//! PostgreSQL adapter for the OXO job store.
//!
//! Claiming uses `SELECT … FOR UPDATE SKIP LOCKED`, PostgreSQL's own idiom
//! for handing one row to exactly one of many concurrent consumers.
//!
//! Every timestamp arrives as a query parameter from the injected
//! [`Clock`](oxo_jobs::Clock). This adapter never calls SQL `now()`: that
//! would put the application and the database on separate clocks, so skew
//! between them would silently change when a job is reclaimed, and it would
//! make expiry testable only by sleeping.

#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use oxo_jobs::clock::Clock;
use oxo_jobs::error::JobStoreError;
use oxo_jobs::ids::RunId;
use oxo_jobs::request::{
    ClaimRequest, ClaimedJob, CreateRun, FailOutcome, FailRequest, Lease, ReapOutcome, ReapRequest,
    RunCreated, RunStatus, Throughput,
};
use oxo_jobs::store::JobStore;
use sqlx::PgPool;

/// Apply the schema. Idempotent; safe to call on every start.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

/// A job store backed by PostgreSQL.
#[derive(Clone)]
pub struct PostgresJobStore {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl PostgresJobStore {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }
}

/// Adapter failures become `JobStoreError::Adapter`, which is the only
/// variant a caller may consider retrying.
fn adapter(error: sqlx::Error) -> JobStoreError {
    JobStoreError::Adapter(error.to_string())
}

#[async_trait]
impl JobStore for PostgresJobStore {
    async fn create_run(&self, _request: CreateRun) -> Result<RunCreated, JobStoreError> {
        unimplemented!("Task 10")
    }
    async fn claim(&self, _request: ClaimRequest) -> Result<Option<ClaimedJob>, JobStoreError> {
        unimplemented!("Task 10")
    }
    async fn heartbeat(&self, _lease: Lease) -> Result<(), JobStoreError> {
        unimplemented!("Task 11")
    }
    async fn complete(&self, _lease: Lease) -> Result<(), JobStoreError> {
        unimplemented!("Task 11")
    }
    async fn fail(&self, _request: FailRequest) -> Result<FailOutcome, JobStoreError> {
        unimplemented!("Task 11")
    }
    async fn reap_expired(&self, _request: ReapRequest) -> Result<ReapOutcome, JobStoreError> {
        unimplemented!("Task 11")
    }
    async fn run_status(&self, _run_id: RunId) -> Result<RunStatus, JobStoreError> {
        unimplemented!("Task 12")
    }
    async fn throughput(&self, _run_id: RunId) -> Result<Throughput, JobStoreError> {
        unimplemented!("Task 12")
    }
}
```

- [ ] **Step 5: Write the failing migration test**

Create `oxo-jobs-postgres/tests/conformance_postgres.rs`:

```rust
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
    oxo_jobs_postgres::run_migrations(&pool)
        .await
        .expect("migrations apply");
    oxo_jobs_postgres::run_migrations(&pool)
        .await
        .expect("migrations are idempotent");

    let tables: Vec<String> =
        sqlx::query_scalar("SELECT tablename FROM pg_tables WHERE schemaname = current_schema()")
            .fetch_all(&pool)
            .await
            .expect("list tables");
    assert!(tables.iter().any(|t| t == "runs"), "{tables:?}");
    assert!(tables.iter().any(|t| t == "jobs"), "{tables:?}");
}
```

- [ ] **Step 6: Run it and verify it fails for the right reason**

Run: `make verify-db`
Expected: the container starts, then FAIL — the migration directory is read but the crate does not compile, or the assertion fails because the schema is absent. Capture the real output. Then make it pass by correcting whatever the failure names.

Run: `make verify`
Expected: PASS, and it must **not** attempt the database tests. Confirm by reading the output: no `conformance_postgres` target should appear.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock Makefile oxo-jobs-postgres/
git commit -F - <<'EOF'
feat(jobs-pg): add the PostgreSQL adapter crate, schema and test harness

Job type and state are text with a CHECK constraint rather than PostgreSQL
enums: an enum would need sqlx::Type on the Rust enums, dragging sqlx into
oxo-jobs and breaking the dependency boundary the crate split exists to
maintain. A further constraint makes "a job holds a lease if and only if
it is claimed" the database's rule, so an adapter bug that leaves a
dangling lease is a write failure rather than silent corruption.

make verify-db starts a disposable PostgreSQL via podman, runs the adapter
tests and tears the container down whether they passed or failed. These
tests are excluded from make verify by package name rather than skipped at
runtime, so a green make verify can never imply the adapter was exercised.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 10: PostgreSQL adapter — `create_run` and `claim`

**Files:**
- Modify: `oxo-jobs-postgres/src/lib.rs`
- Modify: `oxo-jobs-postgres/tests/conformance_postgres.rs`

**Interfaces:**
- Produces: `create_run` and `claim` replacing their stubs, with behaviour identical to the in-memory adapter.

- [ ] **Step 1: Write the fixture and enable the first conformance cases**

Add to `oxo-jobs-postgres/tests/conformance_postgres.rs`:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use oxo_jobs::clock::TestClock;
use oxo_jobs::conformance::{Fixture, Subject};
use oxo_jobs_postgres::PostgresJobStore;
use uuid::Uuid;

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

        let admin = pool().await;
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

        oxo_jobs_postgres::run_migrations(&scoped)
            .await
            .expect("migrations apply");

        let clock = Arc::new(TestClock::new(
            chrono::Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        ));
        Subject {
            store: Box::new(PostgresJobStore::new(scoped, clock.clone())),
            clock,
        }
    }
}

oxo_jobs::conformance_suite!(Postgres);
```

- [ ] **Step 2: Run the suite to verify it fails**

Run: `make verify-db`
Expected: FAIL — every conformance case panics at `unimplemented!("Task 10")` or later. Capture the output. The migration test should still pass.

- [ ] **Step 3: Implement `create_run`**

Replace the stub in `oxo-jobs-postgres/src/lib.rs`:

```rust
    async fn create_run(&self, request: CreateRun) -> Result<RunCreated, JobStoreError> {
        if request.jobs.is_empty() {
            return Err(JobStoreError::EmptyRun {
                region_code: request.region_code,
                revision: request.revision,
            });
        }

        let now = self.clock.now();
        let backoff_secs =
            i64::try_from(request.backoff.as_secs()).unwrap_or(i64::MAX);
        let max_attempts = i32::try_from(request.max_attempts).unwrap_or(i32::MAX);
        let revision = i32::try_from(request.revision).unwrap_or(i32::MAX);

        let mut tx = self.pool.begin().await.map_err(adapter)?;

        // Lock the identity so two concurrent creations cannot both insert.
        let existing: Option<(Uuid, i32, i64)> = sqlx::query_as(
            "SELECT id, max_attempts, backoff_secs FROM runs \
             WHERE region_code = $1 AND revision = $2 FOR UPDATE",
        )
        .bind(&request.region_code)
        .bind(revision)
        .fetch_optional(&mut *tx)
        .await
        .map_err(adapter)?;

        if let Some((run_uuid, existing_attempts, existing_backoff)) = existing {
            let run_id = RunId::from_uuid(run_uuid);
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT tile, job_type FROM jobs WHERE run_id = $1")
                    .bind(run_uuid)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(adapter)?;

            let mut existing_set: Vec<(String, String)> = rows;
            existing_set.sort();
            let mut requested: Vec<(String, String)> = request
                .jobs
                .iter()
                .map(|spec| (spec.tile.to_string(), spec.job_type.as_str().to_string()))
                .collect();
            requested.sort();
            requested.dedup();

            let same_policy =
                existing_attempts == max_attempts && existing_backoff == backoff_secs;
            if existing_set != requested || !same_policy {
                return Err(JobStoreError::RunConflict {
                    region_code: request.region_code,
                    revision: request.revision,
                });
            }

            tx.commit().await.map_err(adapter)?;
            let total_jobs = u32::try_from(existing_set.len()).unwrap_or(u32::MAX);
            return Ok(RunCreated {
                run_id,
                created: false,
                total_jobs,
            });
        }

        let run_uuid = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO runs (id, region_code, revision, max_attempts, backoff_secs, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(run_uuid)
        .bind(&request.region_code)
        .bind(revision)
        .bind(max_attempts)
        .bind(backoff_secs)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(adapter)?;

        for spec in &request.jobs {
            sqlx::query(
                "INSERT INTO jobs (id, run_id, tile, job_type, state, attempts, claimable_at) \
                 VALUES ($1, $2, $3, $4, 'pending', 0, $5) \
                 ON CONFLICT (run_id, tile, job_type) DO NOTHING",
            )
            .bind(Uuid::new_v4())
            .bind(run_uuid)
            .bind(spec.tile.to_string())
            .bind(spec.job_type.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(adapter)?;
        }

        let total_jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE run_id = $1")
            .bind(run_uuid)
            .fetch_one(&mut *tx)
            .await
            .map_err(adapter)?;

        tx.commit().await.map_err(adapter)?;

        Ok(RunCreated {
            run_id: RunId::from_uuid(run_uuid),
            created: true,
            total_jobs: u32::try_from(total_jobs).unwrap_or(u32::MAX),
        })
    }
```

The transaction here is internal to one port call, which the design permits — what it forbids is a transaction *spanning* calls.

- [ ] **Step 4: Implement `claim`**

Replace the stub:

```rust
    async fn claim(&self, request: ClaimRequest) -> Result<Option<ClaimedJob>, JobStoreError> {
        let now = self.clock.now();
        let token = LeaseToken::generate();
        let wanted: Option<Vec<String>> = request.job_types.as_ref().map(|types| {
            types
                .iter()
                .map(|job_type| job_type.as_str().to_string())
                .collect()
        });

        let row: Option<(Uuid, Uuid, String, String, i32)> = sqlx::query_as(
            "UPDATE jobs SET \
                 state = 'claimed', lease_token = $1, claimed_by = $2, \
                 claimed_at = $3, last_heartbeat_at = $3, attempts = attempts + 1 \
             WHERE id = ( \
                 SELECT id FROM jobs \
                 WHERE state = 'pending' AND claimable_at <= $3 \
                   AND ($4::text[] IS NULL OR job_type = ANY($4)) \
                 ORDER BY claimable_at, id \
                 FOR UPDATE SKIP LOCKED \
                 LIMIT 1 \
             ) \
             RETURNING id, run_id, tile, job_type, attempts",
        )
        .bind(token.as_uuid())
        .bind(&request.worker)
        .bind(now)
        .bind(wanted.as_deref())
        .fetch_optional(&self.pool)
        .await
        .map_err(adapter)?;

        let Some((job_uuid, run_uuid, tile, job_type, attempts)) = row else {
            return Ok(None);
        };

        Ok(Some(ClaimedJob {
            job_id: JobId::from_uuid(job_uuid),
            run_id: RunId::from_uuid(run_uuid),
            lease: token,
            tile: tile.parse().map_err(|error| {
                JobStoreError::Adapter(format!("stored tile {tile:?} is not a valid identifier: {error}"))
            })?,
            job_type: JobType::from_str_exact(&job_type).ok_or_else(|| {
                JobStoreError::Adapter(format!("stored job type {job_type:?} is not recognised"))
            })?,
            attempt: u32::try_from(attempts).unwrap_or(u32::MAX),
        }))
    }
```

Add the imports this needs: `oxo_jobs::ids::{JobId, LeaseToken}`, `oxo_jobs::job::JobType`, `uuid::Uuid`.

Note the two `Adapter` errors on conversion. They should be unreachable — the `CHECK` constraint and the specification's validation both prevent them — but a store that silently mangled a tile identifier would be far worse than one that says it found something it could not read.

- [ ] **Step 5: Run the suite**

Run: `make verify-db`
Expected: the creation, claim, filter, empty-queue and concurrency cases PASS; the rest still fail at later `unimplemented!`. **Report which cases pass and which do not** — the concurrency case passing here is the first real evidence `SKIP LOCKED` behaves as intended.

- [ ] **Step 6: Commit**

```bash
git add oxo-jobs-postgres/
git commit -F - <<'EOF'
feat(jobs-pg): create runs idempotently and claim with SKIP LOCKED

create_run locks the (region_code, revision) identity before deciding, so
two concurrent creations cannot both insert, and compares the stored job
set and policy against the request rather than resuming blindly.

Claiming is one statement: the inner SELECT ... FOR UPDATE SKIP LOCKED
picks the oldest claimable job that matches the requested types while
stepping over rows another claimant holds, and the outer UPDATE mints the
lease and spends a start. Timestamps arrive as parameters from the
injected clock; this adapter never calls SQL now().

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 11: PostgreSQL adapter — `heartbeat`, `complete`, `fail`, `reap_expired`

**Files:**
- Modify: `oxo-jobs-postgres/src/lib.rs`

**Interfaces:**
- Produces: the four methods replacing their stubs. The three reporting calls must distinguish `UnknownJob`, `NotClaimed` and `LeaseLost` exactly as the in-memory adapter does — the conformance suite asserts it.

- [ ] **Step 1: Confirm the failing cases**

Run: `make verify-db`
Expected: the lease, failure and reap cases fail at `unimplemented!("Task 11")`. Capture the output.

- [ ] **Step 2: Implement a shared lease resolution**

Add to `impl PostgresJobStore` in `oxo-jobs-postgres/src/lib.rs`:

```rust
    /// Resolve a lease against one job, distinguishing the three ways it can
    /// be invalid. A single query so the checks cannot drift apart between
    /// the three reporting calls, and so the answer cannot change between
    /// two of them.
    async fn resolve<'e, E>(executor: E, lease: Lease) -> Result<(), JobStoreError>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row: Option<(String, Option<Uuid>)> =
            sqlx::query_as("SELECT state, lease_token FROM jobs WHERE id = $1")
                .bind(lease.job_id.as_uuid())
                .fetch_optional(executor)
                .await
                .map_err(adapter)?;

        let Some((state, token)) = row else {
            return Err(JobStoreError::UnknownJob {
                job_id: lease.job_id,
            });
        };
        if state != "claimed" {
            return Err(JobStoreError::NotClaimed {
                job_id: lease.job_id,
            });
        }
        if token != Some(lease.token.as_uuid()) {
            return Err(JobStoreError::LeaseLost {
                job_id: lease.job_id,
            });
        }
        Ok(())
    }
```

- [ ] **Step 3: Implement the three reporting calls**

```rust
    async fn heartbeat(&self, lease: Lease) -> Result<(), JobStoreError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(adapter)?;
        Self::resolve(&mut *tx, lease).await?;
        sqlx::query("UPDATE jobs SET last_heartbeat_at = $1 WHERE id = $2 AND lease_token = $3")
            .bind(now)
            .bind(lease.job_id.as_uuid())
            .bind(lease.token.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(adapter)?;
        tx.commit().await.map_err(adapter)
    }

    async fn complete(&self, lease: Lease) -> Result<(), JobStoreError> {
        let mut tx = self.pool.begin().await.map_err(adapter)?;
        Self::resolve(&mut *tx, lease).await?;
        sqlx::query(
            "UPDATE jobs SET state = 'succeeded', lease_token = NULL, claimed_by = NULL, \
                 claimed_at = NULL, last_heartbeat_at = NULL \
             WHERE id = $1 AND lease_token = $2",
        )
        .bind(lease.job_id.as_uuid())
        .bind(lease.token.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(adapter)?;
        tx.commit().await.map_err(adapter)
    }

    async fn fail(&self, request: FailRequest) -> Result<FailOutcome, JobStoreError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(adapter)?;
        Self::resolve(&mut *tx, request.lease).await?;

        let (attempts, max_attempts, backoff_secs): (i32, i32, i64) = sqlx::query_as(
            "SELECT j.attempts, r.max_attempts, r.backoff_secs \
             FROM jobs j JOIN runs r ON r.id = j.run_id WHERE j.id = $1",
        )
        .bind(request.lease.job_id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(adapter)?;

        let outcome = if attempts >= max_attempts {
            sqlx::query(
                "UPDATE jobs SET state = 'abandoned', lease_token = NULL, claimed_by = NULL, \
                     claimed_at = NULL, last_heartbeat_at = NULL, last_failure = $2 \
                 WHERE id = $1",
            )
            .bind(request.lease.job_id.as_uuid())
            .bind(&request.reason)
            .execute(&mut *tx)
            .await
            .map_err(adapter)?;
            FailOutcome::Abandoned
        } else {
            let claimable_at = now + chrono::Duration::seconds(backoff_secs);
            sqlx::query(
                "UPDATE jobs SET state = 'pending', claimable_at = $2, lease_token = NULL, \
                     claimed_by = NULL, claimed_at = NULL, last_heartbeat_at = NULL, \
                     last_failure = $3 \
                 WHERE id = $1",
            )
            .bind(request.lease.job_id.as_uuid())
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
```

- [ ] **Step 4: Implement `reap_expired`**

```rust
    async fn reap_expired(&self, request: ReapRequest) -> Result<ReapOutcome, JobStoreError> {
        let now = self.clock.now();
        let heartbeat_cutoff = now
            - chrono::Duration::from_std(request.heartbeat_timeout)
                .unwrap_or(chrono::Duration::zero());
        let duration_cutoff = now
            - chrono::Duration::from_std(request.max_job_duration)
                .unwrap_or(chrono::Duration::zero());

        // One statement so a concurrent reaper cannot double-count: each
        // expired row is updated by exactly one of them. The CASE spends the
        // start that was already consumed at claim, abandoning when the
        // budget is gone and requeueing with backoff otherwise.
        let rows: Vec<(String,)> = sqlx::query_as(
            "UPDATE jobs AS j SET \
                 state = CASE WHEN j.attempts >= r.max_attempts THEN 'abandoned' ELSE 'pending' END, \
                 claimable_at = CASE WHEN j.attempts >= r.max_attempts THEN j.claimable_at \
                                     ELSE $1 + (r.backoff_secs * interval '1 second') END, \
                 lease_token = NULL, claimed_by = NULL, claimed_at = NULL, \
                 last_heartbeat_at = NULL \
             FROM runs AS r \
             WHERE r.id = j.run_id AND j.state = 'claimed' \
               AND (j.last_heartbeat_at < $2 OR j.claimed_at < $3) \
             RETURNING j.state",
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
```

`RETURNING j.state` returns the value the `CASE` just assigned, because
`RETURNING` sees the new row — simpler than recomputing the branch and
unambiguously the same answer. Note also that `backoff_secs` is `bigint`
while `make_interval(secs => …)` takes `double precision` with no implicit
cast from `bigint`, which is why the interval is built by multiplication.

- [ ] **Step 5: Run the suite**

Run: `make verify-db`
Expected: all cases except the two gate cases and the throughput case PASS. Report the counts.

- [ ] **Step 6: Commit**

```bash
git add oxo-jobs-postgres/
git commit -F - <<'EOF'
feat(jobs-pg): report on held jobs and reclaim dead ones

The three reporting calls resolve a lease through one shared query, so the
distinction between an unknown job, an unclaimed one and a lost lease
cannot drift apart between them — the conformance suite asserts all three
against both adapters.

Reaping is a single UPDATE ... FROM runs so two concurrent reapers cannot
double-count: each expired row is claimed by exactly one of them. The CASE
spends the start already consumed at claim time, abandoning when the
budget is gone and requeueing with the run's snapshotted backoff
otherwise, so a tile that kills its worker cannot be re-claimed instantly.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 12: PostgreSQL adapter — the gate, throughput, and a green suite

**Files:**
- Modify: `oxo-jobs-postgres/src/lib.rs`

**Interfaces:**
- Produces: `run_status` and `throughput` replacing their stubs. After this task no `unimplemented!` remains in either crate and the whole conformance suite passes against both adapters.

- [ ] **Step 1: Confirm the failing cases**

Run: `make verify-db`
Expected: the three remaining cases fail at `unimplemented!("Task 12")`.

- [ ] **Step 2: Implement both, sharing one tally**

`tally` is an inherent method, so it belongs in the `impl PostgresJobStore`
block alongside `new` and `resolve` — not in `impl JobStore`, which may
contain only the trait's own methods. The two port methods below it go in
`impl JobStore`.

```rust
    /// Count jobs per state for one run, in one query. Shared by the gate
    /// and the throughput snapshot so the two cannot disagree.
    async fn tally(&self, run_id: RunId, now: chrono::DateTime<chrono::Utc>) -> Result<Throughput, JobStoreError> {
        let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM runs WHERE id = $1")
            .bind(run_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(adapter)?;
        if exists.is_none() {
            return Err(JobStoreError::UnknownRun { run_id });
        }

        let (pending, claimable_now, claimed, succeeded, abandoned): (i64, i64, i64, i64, i64) =
            sqlx::query_as(
                "SELECT \
                     count(*) FILTER (WHERE state = 'pending'), \
                     count(*) FILTER (WHERE state = 'pending' AND claimable_at <= $2), \
                     count(*) FILTER (WHERE state = 'claimed'), \
                     count(*) FILTER (WHERE state = 'succeeded'), \
                     count(*) FILTER (WHERE state = 'abandoned') \
                 FROM jobs WHERE run_id = $1",
            )
            .bind(run_id.as_uuid())
            .bind(now)
            .fetch_one(&self.pool)
            .await
            .map_err(adapter)?;

        let count = |value: i64| u32::try_from(value).unwrap_or(u32::MAX);
        Ok(Throughput {
            pending: count(pending),
            claimable_now: count(claimable_now),
            claimed: count(claimed),
            succeeded: count(succeeded),
            abandoned: count(abandoned),
        })
    }
```

Then the two methods:

```rust
    async fn run_status(&self, run_id: RunId) -> Result<RunStatus, JobStoreError> {
        let now = self.clock.now();
        let tally = self.tally(run_id, now).await?;

        if tally.pending == 0 && tally.claimed == 0 {
            return Ok(if tally.abandoned == 0 {
                RunStatus::Complete
            } else {
                RunStatus::Failed {
                    abandoned: tally.abandoned,
                }
            });
        }

        Ok(RunStatus::InProgress {
            pending: tally.pending,
            claimed: tally.claimed,
            succeeded: tally.succeeded,
            abandoned: tally.abandoned,
        })
    }

    async fn throughput(&self, run_id: RunId) -> Result<Throughput, JobStoreError> {
        let now = self.clock.now();
        self.tally(run_id, now).await
    }
```

- [ ] **Step 3: Run the whole suite against both adapters**

Run: `make verify`
Expected: PASS, including the 15 in-memory conformance tests.

Run: `make verify-db`
Expected: PASS, all 15 conformance cases plus the migration test — 16 tests against a real PostgreSQL.

- [ ] **Step 4: Confirm no stubs survive**

```bash
grep -rn 'unimplemented!' oxo-jobs/src oxo-jobs-postgres/src
```
Expected: no output. If anything matches, a method was never implemented and the suite is not covering it — report that rather than removing the grep.

- [ ] **Step 5: Confirm the dependency boundary held**

```bash
cargo tree --package oxo-jobs --edges normal | grep -iE "sqlx|postgres|tokio-postgres|reqwest|hyper"
```
Expected: no output. The whole point of the crate split is that a consumer can depend on the port without the adapter's dependencies. If `sqlx` has reached `oxo-jobs`, the design is broken — stop and report it.

- [ ] **Step 6: Commit**

```bash
git add oxo-jobs-postgres/
git commit -F - <<'EOF'
feat(jobs-pg): answer the completion gate and the throughput snapshot

Both read one aggregate query with FILTER clauses, so the gate and the
snapshot cannot disagree about a run. An unknown run is refused rather
than reported as an empty-and-therefore-complete one.

The conformance suite now passes in full against both adapters: fifteen
invariants, identical cases, one in memory and one against a real
PostgreSQL. That is what makes the in-memory adapter trustworthy as a test
double rather than a convenient fiction.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

## Completion

When all twelve tasks are done:

- `make verify` passes, including 15 in-memory conformance tests.
- `make verify-db` passes, including the same 15 against a real PostgreSQL.
- No `unimplemented!` remains in either crate.
- `cargo tree --package oxo-jobs --edges normal` shows no database or network dependency.

Five design-document open decisions are deliberately untouched:

- **The per-tile resource footprint model** — awaits spike 0's measurements. The job-type filter is the interim capacity hook; the estimate later becomes a column and a claim predicate, which is additive.
- **Claim fairness across concurrent runs** — first-in-first-out by `claimable_at` across all runs means an older run starves a newer one. Acceptable while one region is produced at a time.
- **Whether a reap should ever be exempt from consuming an attempt** — a node reboot and an out-of-memory kill are indistinguishable today.
- **Whether a do-not-retry hint earns its place** — an added optional field if sub-project 4's entry point turns out to recognise terminal cases.
- **The exact metric set behind `throughput`** — a snapshot of counts here; rates are the consumer's to derive by differencing successive snapshots, and the shape should settle with sub-project 3 as the first real consumer.

Two things sub-project 3 inherits and must honour:

- **Construct a `RegionSpec` only via `validate`/`from_toml`.** Nothing in the type system enforces it.
- **`reap_expired` is not self-driving.** Something must call it periodically, and that something is the control plane. A store whose reaper is never invoked never reclaims anything.
