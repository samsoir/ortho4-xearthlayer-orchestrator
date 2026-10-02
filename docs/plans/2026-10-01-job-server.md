# Job Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A durable task store that hands each unit of tile production to exactly one worker, reclaims work from workers that die, retries under a declared policy, and can state whether a region is finished — behind a port with an in-memory and a PostgreSQL adapter.

**Architecture:** Two crates. `oxo-tasks` carries the port trait, its owned request/response types, the task state machine and an in-memory adapter, with no database dependency at all. `oxo-tasks-postgres` carries the PostgreSQL adapter. One conformance suite, shipped from `oxo-tasks` behind a feature flag, runs against both adapters so the in-memory one cannot drift into a convenient fiction.

**Tech Stack:** Rust (edition 2021), `async-trait` for a dyn-compatible port, `tokio` for the runtime, `chrono` for timestamps, `uuid` for identities, `sqlx` (PostgreSQL, in the adapter crate only), `thiserror` for error types, podman for a disposable test database.

**Spec:** `docs/specs/2026-10-01-job-server-design.md` (and the architecture it inherits, `docs/specs/2026-10-01-oxo-architecture-design.md`)

## Global Constraints

Every task's requirements implicitly include this section.

- **TDD is mandatory.** Write the failing test, run it and capture the real failing output, then write the minimal code to pass. A report without per-cycle RED output is incomplete.
- **SOLID.** This sub-project is where the project's dependency-inversion constraint finally has something real to invert: consumers depend on the `TaskStore` trait, never on a concrete adapter.
- **Rust for all code.** Test coverage target 80% minimum, 90%+ aspirational; not measured in CI, which does not yet exist.
- **`clippy` with `-D warnings` and tests with `RUSTFLAGS="-D warnings"`** via `make verify`, so warnings are build failures and output must be pristine.
- **`oxo-tasks` must not depend on `sqlx`, any database driver, or any network or filesystem crate.** The whole point of the split is that a consumer can depend on the port without the adapter's dependencies entering its graph. A task that adds such a dependency to `oxo-tasks` has broken the design.
- **Every timestamp comes from an injected `Clock`.** No `SystemTime::now()` or `Utc::now()` anywhere outside `SystemClock`, and the PostgreSQL adapter never calls SQL `now()` — timestamps travel as query parameters. This is what makes expiry deterministically testable against both adapters and removes application-versus-database clock skew.
- **Every port method takes an owned request and returns an owned response, with no transaction spanning a call.** This keeps a future network adapter mechanical. It rules out exposing `begin → select → update → commit` across the API.
- **A lease token is required by `heartbeat`, `complete` and `fail`,** and a mismatch is `TaskStoreError::LeaseLost` — never silently ignored, never a generic error.
- **`attempts` counts starts, not failures.** It increments at claim time. `max_attempts` bounds starts.
- **A reaped task takes the same backoff as a reported failure.**
- **Commit messages end with** `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`, preceded by a blank line or git parses no trailer. Verify with `git log -1 --format='%s'` (subject alone) and `git log -1 --format='%(trailers)'`.
- **After committing, confirm what you committed.** `git status --porcelain` must be empty and `git show HEAD:<path>` must contain your change. A task in the previous sub-project passed `make verify` against its working tree while the commit held none of it.

### Vocabulary

| Term | Meaning |
|---|---|
| Job | One submission of one specification revision. Identified by `(region_code, revision)`. |
| Task | One unit of work: one tile, one task type, within one job. |
| Task type | `Ortho` or `Overlay`. A tile may have both; they are independent tasks. |
| Lease | The right to work on a claimed task, carried by a token minted at claim. |
| Attempt | A start. Incremented when a task is claimed, never when it fails. |
| Reap | Reclaiming a claimed task whose heartbeat lapsed or which exceeded the maximum duration. |

**A collision to keep straight.** This plan's own numbered units of work are
called Tasks — Task 1, Task 2 and so on — because the tooling that executes
it requires that heading. The domain's unit of tile work is also a task. They
are distinguished by capitalisation and number: "Task 7" is a unit of this
plan, "a task" is one tile's ortho or overlay conversion. The
`unimplemented!("Task 5")` markers refer to plan Tasks.

---

### Task 1: `oxo-tasks` crate, identities, and the clock

**Files:**
- Modify: `Cargo.toml`
- Create: `oxo-tasks/Cargo.toml`
- Create: `oxo-tasks/src/lib.rs`
- Create: `oxo-tasks/src/clock.rs`
- Create: `oxo-tasks/src/ids.rs`

**Interfaces:**
- Produces: `Clock` (trait, `fn now(&self) -> DateTime<Utc>`), `SystemClock`, `TestClock` (`new`, `advance`); `JobId`, `TaskId`, `LeaseToken` — each a `Uuid` newtype with `generate()`, `from_uuid()`, `as_uuid()`, `Display`, and `Copy + Eq + Hash + Ord`.

- [ ] **Step 1: Add the crate to the workspace**

Modify `Cargo.toml` so `[workspace]` and `[workspace.dependencies]` read:

```toml
[workspace]
members = ["oxo-spec", "oxo-spec-cli", "oxo-tasks"]
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

Create `oxo-tasks/Cargo.toml`:

```toml
[package]
name = "oxo-tasks"
description = "Task store port, state machine and in-memory adapter for the Ortho4 XEarthLayer Orchestrator"
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

Create `oxo-tasks/src/clock.rs`:

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

Create `oxo-tasks/src/ids.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identities_are_distinct() {
        assert_ne!(JobId::generate(), JobId::generate());
        assert_ne!(TaskId::generate(), TaskId::generate());
        assert_ne!(LeaseToken::generate(), LeaseToken::generate());
    }

    #[test]
    fn an_identity_round_trips_through_its_uuid() {
        let id = TaskId::generate();
        assert_eq!(TaskId::from_uuid(id.as_uuid()), id);
    }

    #[test]
    fn an_identity_displays_as_its_uuid() {
        let id = JobId::generate();
        assert_eq!(id.to_string(), id.as_uuid().to_string());
    }

    // There is deliberately no test asserting that the three identities
    // are distinct *types*. That is a compile-time property: the macro
    // emits three separate nominal structs, so passing a TaskId where a
    // JobId belongs does not compile, and `cargo build` already enforces
    // it. A `#[test]` wrapping a call that merely compiles asserts
    // nothing at runtime and can never fail — the same shape as three
    // tests removed from Task 2 during this plan's pre-flight scan.
}
```

Create `oxo-tasks/src/lib.rs`:

```rust
//! Task store port, state machine and in-memory adapter for the Ortho4
//! XEarthLayer Orchestrator.
//!
//! This crate has no database dependency. Consumers depend on the
//! [`TaskStore`] port; a concrete adapter is named only at the composition
//! root. Every timestamp comes from an injected [`Clock`], so expiry and
//! backoff are deterministically testable and the application never
//! disagrees with a database about what time it is.

#![forbid(unsafe_code)]

pub mod clock;
pub mod ids;

pub use clock::{Clock, SystemClock, TestClock};
pub use ids::{TaskId, LeaseToken, JobId};
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --package oxo-tasks`
Expected: FAIL to compile — `cannot find type TestClock`, `cannot find type JobId`.

- [ ] **Step 4: Implement the clock**

Prepend to `oxo-tasks/src/clock.rs`:

```rust
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Source of time for the task store.
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

Prepend to `oxo-tasks/src/ids.rs`:

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

// Three separate invocations, so these are three distinct nominal
// types: passing a TaskId where a JobId belongs will not compile.
// Enforced by the type system, not by a test.
identity!(JobId, "Identifies one job: one submission of one specification revision.");
identity!(TaskId, "Identifies one task: one tile, one task type, within one job.");
identity!(
    LeaseToken,
    "Proves the right to report on a claimed task. Minted fresh at every claim, so a worker whose task was reclaimed cannot report on it."
);
```

A macro is used here rather than three hand-written copies because the three types are identical in every respect except their name and documentation; writing them out would be the verbatim duplication the review rubric treats as a defect.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --package oxo-tasks`
Expected: PASS, 7 tests.

- [ ] **Step 7: Verify the dependency boundary**

Run: `cargo tree --package oxo-tasks --edges normal | grep -iE "sqlx|postgres|tokio-postgres|reqwest|hyper"`
Expected: no output. If anything matches, a database or network dependency has entered `oxo-tasks` and the design is broken — stop and report it.

- [ ] **Step 8: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add Cargo.toml Cargo.lock oxo-tasks/
git commit -F - <<'EOF'
feat(tasks): add the oxo-tasks crate, its identities and an injected clock

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
- Create: `oxo-tasks/src/task.rs`
- Create: `oxo-tasks/src/request.rs`
- Create: `oxo-tasks/src/error.rs`
- Modify: `oxo-tasks/src/lib.rs`

**Interfaces:**
- Consumes: `TaskId`, `JobId`, `LeaseToken`, `oxo_spec::TileId`.
- Produces: `TaskType { Ortho, Overlay }`; `TaskState { Pending, Claimed, Succeeded, Abandoned }`; `TaskSpec { tile, task_type }`; `CreateJob { region_code, revision, max_attempts, backoff, tasks }`; `JobCreated { job_id, created, total_tasks }`; `ClaimRequest { worker, task_types }`; `ClaimedTask { task_id, job_id, lease, tile, task_type, attempt }`; `Lease { task_id, token }`; `FailRequest { lease, reason }`; `FailOutcome { Requeued { claimable_at, attempts_remaining }, Abandoned }`; `ReapRequest { heartbeat_timeout, max_task_duration }`; `ReapOutcome { requeued, abandoned }`; `JobStatus { Complete, Failed { abandoned }, InProgress { pending, claimed, succeeded, abandoned } }`; `Throughput { pending, claimable_now, claimed, succeeded, abandoned }`; `TaskStoreError`.

- [ ] **Step 1: Write the failing tests**

Create `oxo-tasks/src/task.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_types_are_distinct_and_namable() {
        assert_ne!(TaskType::Ortho, TaskType::Overlay);
        assert_eq!(TaskType::Ortho.as_str(), "ortho");
        assert_eq!(TaskType::Overlay.as_str(), "overlay");
    }

    #[test]
    fn task_type_round_trips_through_its_string_form() {
        for task_type in [TaskType::Ortho, TaskType::Overlay] {
            assert_eq!(TaskType::from_str_exact(task_type.as_str()), Some(task_type));
        }
        assert_eq!(TaskType::from_str_exact("ORTHO"), None);
        assert_eq!(TaskType::from_str_exact("mesh"), None);
    }

    #[test]
    fn terminal_states_are_marked_as_such() {
        assert!(!TaskState::Pending.is_terminal());
        assert!(!TaskState::Claimed.is_terminal());
        assert!(TaskState::Succeeded.is_terminal());
        assert!(TaskState::Abandoned.is_terminal());
    }

    #[test]
    fn task_state_round_trips_through_its_string_form() {
        for state in [
            TaskState::Pending,
            TaskState::Claimed,
            TaskState::Succeeded,
            TaskState::Abandoned,
        ] {
            assert_eq!(TaskState::from_str_exact(state.as_str()), Some(state));
        }
        assert_eq!(TaskState::from_str_exact("running"), None);
        // A case variant, specifically: this is the assertion that would
        // catch an accidental `.to_lowercase()` creeping into the parse.
        // TaskType's round-trip test pins the same rule; without this line
        // TaskState's exact-match guarantee is unasserted.
        assert_eq!(TaskState::from_str_exact("PENDING"), None);
    }
}
```

Create `oxo-tasks/src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TaskId;

    #[test]
    fn a_lost_lease_says_so_and_names_the_task() {
        let task_id = TaskId::generate();
        let error = TaskStoreError::LeaseLost { task_id };
        let rendered = error.to_string();
        assert!(rendered.contains("lease"), "{rendered}");
        assert!(rendered.contains(&task_id.to_string()), "{rendered}");
    }

    #[test]
    fn a_run_conflict_names_the_identity_that_collided() {
        let error = TaskStoreError::JobConflict {
            region_code: "NA".to_string(),
            revision: 2,
        };
        let rendered = error.to_string();
        assert!(rendered.contains("NA"), "{rendered}");
        assert!(rendered.contains('2'), "{rendered}");
    }

    #[test]
    fn every_variant_renders_something_an_operator_can_act_on() {
        let task_id = TaskId::generate();
        let job_id = crate::ids::JobId::generate();
        let cases: Vec<(TaskStoreError, &str)> = vec![
            (TaskStoreError::LeaseLost { task_id }, "lease"),
            (TaskStoreError::UnknownJob { job_id }, "job"),
            (TaskStoreError::UnknownTask { task_id }, "task"),
            (TaskStoreError::NotClaimed { task_id }, "not claimed"),
            (
                TaskStoreError::JobConflict {
                    region_code: "OC".to_string(),
                    revision: 1,
                },
                "already exists",
            ),
            (
                TaskStoreError::Adapter("connection reset".to_string()),
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

Create `oxo-tasks/src/request.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tile(lat: i8, lon: i16) -> TileId {
        TileId::new(lat, lon).expect("in range")
    }

    #[test]
    fn a_create_job_request_carries_its_whole_task_set() {
        let request = CreateJob {
            region_code: "NA".to_string(),
            revision: 1,
            max_attempts: 3,
            backoff: Duration::from_secs(60),
            tasks: vec![
                TaskSpec {
                    tile: tile(50, -2),
                    task_type: TaskType::Ortho,
                },
                TaskSpec {
                    tile: tile(50, -2),
                    task_type: TaskType::Overlay,
                },
            ],
        };
        assert_eq!(request.tasks.len(), 2);
        assert_eq!(request.tasks[0].tile, request.tasks[1].tile);
        assert_ne!(request.tasks[0].task_type, request.tasks[1].task_type);
    }

    // Only one test here, and deliberately so. These are plain data
    // definitions, so the red-green driver is compilation: before the types
    // exist this module does not compile, and after they do it does. Three
    // further tests were drafted and removed during the pre-flight scan —
    // two asserted that distinct enum variants differ, which can never
    // fail, one asserted that a field just set to `None` is `None`, and one
    // of them called `Utc::now()` directly, which the Global Constraints
    // forbid. Behavioural coverage of every one of these types arrives in
    // Tasks 4 through 8, which assert concrete values such as
    // `FailOutcome::Abandoned` and `JobStatus::Complete`.
}
```

**This task's RED is a compile failure, not an assertion failure.** That is
legitimate for pure data definitions: the test module cannot compile until
the types exist. Say so plainly in your report and paste the compiler error
— do not invent an assertion that fails for show.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-tasks`
Expected: FAIL to compile — `TaskType`, `TaskStoreError`, `CreateJob` not found.

- [ ] **Step 3: Implement the domain types**

Prepend to `oxo-tasks/src/task.rs`:

```rust
/// The two independent kinds of work a tile needs.
///
/// They are separate tasks because they share no data, their resource
/// profiles differ by orders of magnitude, and their dependencies are
/// disjoint — an imagery provider and Overpass for ortho, an X-Plane
/// overlay source and DSFTool for overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum TaskType {
    Ortho,
    Overlay,
}

impl TaskType {
    /// The canonical lowercase name, used as the database enum label.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ortho => "ortho",
            Self::Overlay => "overlay",
        }
    }

    /// Parse the canonical form. Exact match only — no case folding, so a
    /// database label and this enum cannot drift apart silently.
    ///
    /// The reason these string forms exist at all: the PostgreSQL schema
    /// stores task type and state as `text` with a `CHECK` constraint rather
    /// than as PostgreSQL enums. A PostgreSQL enum would require
    /// `#[derive(sqlx::Type)]` on this enum, which would pull `sqlx` into
    /// `oxo-tasks` and break the crate split that lets a consumer depend on
    /// the port without a database driver. Do not "improve" this to a
    /// PostgreSQL enum without reading that decision first.
    pub fn from_str_exact(text: &str) -> Option<Self> {
        match text {
            "ortho" => Some(Self::Ortho),
            "overlay" => Some(Self::Overlay),
            _ => None,
        }
    }

    /// Every variant, for exhaustive iteration in tests and queries.
    pub const ALL: [TaskType; 2] = [TaskType::Ortho, TaskType::Overlay];
}

/// Where a task is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TaskState {
    /// Claimable once `claimable_at` has passed.
    Pending,
    /// Held under a lease, expected to heartbeat.
    Claimed,
    Succeeded,
    /// Retries exhausted. The job can never complete.
    Abandoned,
}

impl TaskState {
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

Prepend to `oxo-tasks/src/error.rs`:

```rust
use thiserror::Error;

use crate::ids::{TaskId, JobId};

/// Why a task store operation did not succeed.
///
/// The first five are deterministic outcomes of a correct store and must be
/// representable without a caller parsing a string. Only [`Adapter`] may be
/// worth retrying.
///
/// [`Adapter`]: TaskStoreError::Adapter
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TaskStoreError {
    /// The lease token does not match the one the task was claimed with. The
    /// caller has lost this task — another worker may already hold it — and
    /// must stop rather than retry.
    #[error("lease for task {task_id} is no longer held; it was reclaimed and must not be reported on")]
    LeaseLost { task_id: TaskId },

    #[error("no such job {job_id}")]
    UnknownJob { job_id: JobId },

    #[error("no such task {task_id}")]
    UnknownTask { task_id: TaskId },

    #[error("task {task_id} is not claimed, so it cannot be completed or failed")]
    NotClaimed { task_id: TaskId },

    /// A job for this identity already exists with a different task set or
    /// policy. Resuming it would silently run something other than what was
    /// asked for.
    #[error(
        "a job for region {region_code} revision {revision} already exists with a different task set or failure policy"
    )]
    JobConflict { region_code: String, revision: u32 },

    #[error("task store adapter failed: {0}")]
    Adapter(String),
}
```

- [ ] **Step 5: Implement the request and response types**

Prepend to `oxo-tasks/src/request.rs`:

```rust
use std::time::Duration;

use chrono::{DateTime, Utc};
use oxo_spec::TileId;

use crate::ids::{TaskId, LeaseToken, JobId};
use crate::task::TaskType;

/// One unit of work within a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    pub tile: TileId,
    pub task_type: TaskType,
}

/// Register a job and the whole task set it consists of.
///
/// The task set arrives already computed: atomizing a specification into up
/// to 2N tasks is the planner's work, not the store's. The failure policy is
/// snapshotted onto the job, so editing a specification later cannot change
/// the policy of a job already in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateJob {
    pub region_code: String,
    pub revision: u32,
    pub max_attempts: u32,
    pub backoff: Duration,
    pub tasks: Vec<TaskSpec>,
}

/// The outcome of registering a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobCreated {
    pub job_id: JobId,
    /// `false` when a job for this identity already existed and this call
    /// resumed it rather than creating anything.
    pub created: bool,
    pub total_tasks: u32,
}

/// Ask for one task to work on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRequest {
    /// Worker identity, recorded so an operator can see who holds what.
    pub worker: String,
    /// Restrict to these task types. `None` means any.
    ///
    /// This is how a worker expresses capacity until there is a footprint
    /// model: one short on disk claims overlay work only, an overlay task
    /// being a file copy and a conversion where an ortho task is hundreds of
    /// gigabytes.
    pub task_types: Option<Vec<TaskType>>,
}

/// A task handed to a worker, with the lease that proves it is theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedTask {
    pub task_id: TaskId,
    pub job_id: JobId,
    pub lease: LeaseToken,
    pub tile: TileId,
    pub task_type: TaskType,
    /// Which start this is, counting from 1.
    pub attempt: u32,
}

/// Proof that the caller holds a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub task_id: TaskId,
    pub token: LeaseToken,
}

/// Report that a task failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailRequest {
    pub lease: Lease,
    /// Whatever the worker could determine. Recorded, never interpreted —
    /// Ortho4XP's headless path reports a bare `Crash!`, so the store draws
    /// no conclusions from this text.
    pub reason: String,
}

/// What became of a failed task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    Requeued {
        claimable_at: DateTime<Utc>,
        attempts_remaining: u32,
    },
    Abandoned,
}

/// Reclaim tasks whose workers have gone away.
///
/// Both bounds are server configuration rather than per-job or
/// per-specification: they are operational tuning, and an operator
/// authoring a region is the person least placed to choose them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReapRequest {
    /// Reclaim a claimed task if no heartbeat has arrived within this.
    pub heartbeat_timeout: Duration,
    /// Reclaim a claimed task once it has been held this long regardless of
    /// heartbeats, covering a worker that is wedged but alive.
    pub max_task_duration: Duration,
}

/// What a reap did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReapOutcome {
    pub requeued: u32,
    /// Reclaimed tasks whose attempts were already exhausted.
    pub abandoned: u32,
}

/// Whether a region is finished — the completion gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Complete,
    /// Nothing left to run, and at least one task abandoned. The region can
    /// never complete.
    Failed { abandoned: u32 },
    /// Work remains. `abandoned` is reported here too, deliberately: a job
    /// with an abandoned task is already unachievable, and an operator should
    /// learn that in minutes rather than after a fortnight.
    InProgress {
        pending: u32,
        claimed: u32,
        succeeded: u32,
        abandoned: u32,
    },
}

/// A snapshot of a job's queue.
///
/// Counts, not rates: rates need two observations, and differencing
/// successive snapshots is the consumer's task. The metric set is an open
/// decision in the design document, to be settled with the first real
/// consumer rather than guessed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Throughput {
    pub pending: u32,
    /// Pending tasks whose backoff has elapsed, so claimable right now.
    pub claimable_now: u32,
    pub claimed: u32,
    pub succeeded: u32,
    pub abandoned: u32,
}
```

- [ ] **Step 6: Wire the modules**

Add to `oxo-tasks/src/lib.rs`, keeping alphabetical grouping:

```rust
pub mod error;
pub mod task;
pub mod request;

pub use error::TaskStoreError;
pub use task::{TaskState, TaskType};
pub use request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, TaskSpec, Lease, ReapOutcome,
    ReapRequest, JobCreated, JobStatus, Throughput,
};
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --package oxo-tasks`
Expected: PASS, 15 tests.

- [ ] **Step 8: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add oxo-tasks/
git commit -F - <<'EOF'
feat(tasks): add the domain types and the port's request surface

Task types and states with exact string forms, so a database enum label
and the Rust enum cannot drift apart silently. Owned request and response
types for every port method, which is what keeps a future network adapter
mechanical rather than a redesign.

TaskStoreError distinguishes the five deterministic outcomes of a correct
store from an adapter failure, so a caller never parses a string to learn
whether retrying is sensible. LeaseLost says outright that the caller has
lost the task and must stop.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 3: The `TaskStore` port

**Files:**
- Create: `oxo-tasks/src/store.rs`
- Modify: `oxo-tasks/src/lib.rs`

**Interfaces:**
- Consumes: every type from Task 2.
- Produces: `#[async_trait] pub trait TaskStore: Send + Sync` with the eight methods below, and a compile-time assertion that it is dyn-compatible.

- [ ] **Step 1: Write the failing test**

Create `oxo-tasks/src/store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The composition root injects an adapter behind a trait object, so
    /// losing dyn-compatibility would break the design. This fails to
    /// compile rather than at runtime if that happens.
    #[test]
    fn the_port_is_dyn_compatible() {
        fn assert_dyn_compatible(_: Option<&dyn TaskStore>) {}
        assert_dyn_compatible(None);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --package oxo-tasks store`
Expected: FAIL to compile — `cannot find trait TaskStore`.

- [ ] **Step 3: Implement the port**

Prepend to `oxo-tasks/src/store.rs`:

```rust
use async_trait::async_trait;

use crate::error::TaskStoreError;
use crate::ids::JobId;
use crate::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, Lease, ReapOutcome, ReapRequest,
    JobCreated, JobStatus, Throughput,
};

/// Durable task state, leasing, retry accounting and the completion gate.
///
/// Every method takes an owned request and returns an owned response, and
/// no transaction spans a call. That is deliberate: it keeps a network
/// adapter a mechanical addition rather than a redesign, and it rules out
/// exposing `begin → select → update → commit` across this boundary.
///
/// `async_trait` is used so the port is dyn-compatible — the composition
/// root injects an adapter behind a trait object.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Register a job and its task set. Idempotent on
    /// `(region_code, revision)`: calling it again with the same task set
    /// and policy resumes the existing job and reports `created: false`.
    /// Calling it with the same identity but a different task set or policy
    /// is [`TaskStoreError::JobConflict`].
    async fn create_job(&self, request: CreateJob) -> Result<JobCreated, TaskStoreError>;

    /// Hand one claimable task to a worker, minting a fresh lease token.
    /// `Ok(None)` means nothing is claimable, which is not an error.
    ///
    /// Increments the task's attempt count. Attempts count starts, so a task
    /// reclaimed from a dead worker has already consumed one.
    async fn claim(&self, request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError>;

    /// Assert that a lease is still held. Returns
    /// [`TaskStoreError::LeaseLost`] if the task was reclaimed, which tells
    /// the worker to stop working.
    async fn heartbeat(&self, lease: Lease) -> Result<(), TaskStoreError>;

    /// Mark a claimed task succeeded.
    async fn complete(&self, lease: Lease) -> Result<(), TaskStoreError>;

    /// Record a failure, requeueing after backoff or abandoning if the
    /// attempt budget is spent.
    async fn fail(&self, request: FailRequest) -> Result<FailOutcome, TaskStoreError>;

    /// Reclaim claimed tasks whose heartbeat has lapsed or which have
    /// exceeded the maximum duration. A reaped task takes the same backoff
    /// as a reported failure, so a tile that kills its worker cannot be
    /// re-claimed instantly and burn its budget in minutes.
    async fn reap_expired(&self, request: ReapRequest) -> Result<ReapOutcome, TaskStoreError>;

    /// The completion gate.
    async fn job_status(&self, job_id: JobId) -> Result<JobStatus, TaskStoreError>;

    /// A snapshot of the job's queue.
    async fn throughput(&self, job_id: JobId) -> Result<Throughput, TaskStoreError>;
}
```

- [ ] **Step 4: Wire the module**

Add to `oxo-tasks/src/lib.rs`:

```rust
pub mod store;

pub use store::TaskStore;
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --package oxo-tasks store`
Expected: PASS, 1 test.

- [ ] **Step 6: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add oxo-tasks/
git commit -F - <<'EOF'
feat(tasks): define the TaskStore port

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

### Task 4: In-memory adapter — state and `create_job`

**Files:**
- Create: `oxo-tasks/src/memory.rs`
- Modify: `oxo-tasks/src/lib.rs`

**Interfaces:**
- Consumes: `Clock`, every type from Task 2, the `TaskStore` trait.
- Produces: `InMemoryTaskStore::new(clock: Arc<dyn Clock>)`, implementing `create_job`. Remaining methods are added by Tasks 5-7; until then they return `unimplemented!()` with a comment naming the task that fills them.

- [ ] **Step 1: Write the failing tests**

Create `oxo-tasks/src/memory.rs`:

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

    pub(crate) fn store(clock: Arc<TestClock>) -> InMemoryTaskStore {
        InMemoryTaskStore::new(clock)
    }

    pub(crate) fn two_tile_job() -> CreateJob {
        CreateJob {
            region_code: "NA".to_string(),
            revision: 1,
            max_attempts: 3,
            backoff: Duration::from_secs(60),
            tasks: vec![
                TaskSpec {
                    tile: tile(50, -2),
                    task_type: TaskType::Ortho,
                },
                TaskSpec {
                    tile: tile(50, -2),
                    task_type: TaskType::Overlay,
                },
            ],
        }
    }

    #[tokio::test]
    async fn creating_a_run_reports_what_it_created() {
        let store = store(clock());
        let created = store.create_job(two_tile_job()).await.expect("create");
        assert!(created.created);
        assert_eq!(created.total_tasks, 2);
    }

    #[tokio::test]
    async fn creating_the_same_run_again_resumes_rather_than_duplicating() {
        let store = store(clock());
        let first = store.create_job(two_tile_job()).await.expect("create");
        let second = store.create_job(two_tile_job()).await.expect("resume");

        assert!(first.created);
        assert!(!second.created, "the second call must not have created anything");
        assert_eq!(first.job_id, second.job_id);
        assert_eq!(second.total_tasks, 2);
    }

    #[tokio::test]
    async fn a_different_task_set_under_the_same_identity_is_a_conflict() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let mut altered = two_tile_job();
        altered.tasks.push(TaskSpec {
            tile: tile(51, -2),
            task_type: TaskType::Ortho,
        });

        let error = store.create_job(altered).await.expect_err("should conflict");
        assert!(
            matches!(error, TaskStoreError::JobConflict { .. }),
            "expected JobConflict, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_different_failure_policy_under_the_same_identity_is_a_conflict() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let mut altered = two_tile_job();
        altered.max_attempts = 5;

        let error = store.create_job(altered).await.expect_err("should conflict");
        assert!(
            matches!(error, TaskStoreError::JobConflict { .. }),
            "expected JobConflict, got {error:?}"
        );
    }

    #[tokio::test]
    async fn the_task_set_is_compared_without_regard_to_order() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let mut reordered = two_tile_job();
        reordered.tasks.reverse();

        let resumed = store.create_job(reordered).await.expect("resume");
        assert!(!resumed.created, "reordering the task set is not a different job");
    }

    #[tokio::test]
    async fn two_revisions_of_one_region_are_separate_runs() {
        let store = store(clock());
        let first = store.create_job(two_tile_job()).await.expect("create");

        let mut next = two_tile_job();
        next.revision = 2;
        let second = store.create_job(next).await.expect("create");

        assert_ne!(first.job_id, second.job_id);
        assert!(second.created);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-tasks memory`
Expected: FAIL to compile — `cannot find type InMemoryTaskStore`.

- [ ] **Step 3: Implement the state and `create_job`**

Prepend to `oxo-tasks/src/memory.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use oxo_spec::TileId;

use crate::clock::{Clock, TestClock};
use crate::error::TaskStoreError;
use crate::ids::{TaskId, LeaseToken, JobId};
use crate::task::{TaskState, TaskType};
use crate::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, TaskSpec, Lease, ReapOutcome,
    ReapRequest, JobCreated, JobStatus, Throughput,
};
use crate::store::TaskStore;

/// A task store held entirely in memory.
///
/// Exists so the port can be exercised without a database, and so the
/// conformance suite has a second implementation to hold the PostgreSQL
/// adapter honest. Not durable; not intended for production.
///
/// The mutex is never held across an await — every operation is synchronous
/// once inside it — so a `std` mutex is correct here and simpler than an
/// async one.
#[derive(Debug)]
pub struct InMemoryTaskStore {
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    jobs: BTreeMap<JobId, Job>,
    by_identity: BTreeMap<(String, u32), JobId>,
    tasks: BTreeMap<TaskId, Task>,
}

#[derive(Debug)]
struct Job {
    max_attempts: u32,
    backoff: Duration,
    task_ids: Vec<TaskId>,
}

#[derive(Debug)]
struct Task {
    job_id: JobId,
    tile: TileId,
    task_type: TaskType,
    state: TaskState,
    attempts: u32,
    claimable_at: DateTime<Utc>,
    lease: Option<LeaseToken>,
    claimed_by: Option<String>,
    claimed_at: Option<DateTime<Utc>>,
    last_heartbeat_at: Option<DateTime<Utc>>,
    last_failure: Option<String>,
}

impl InMemoryTaskStore {
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

/// The identity of a task within its job, used to compare task sets.
fn task_key(spec: &TaskSpec) -> (TileId, TaskType) {
    (spec.tile, spec.task_type)
}

#[async_trait]
impl TaskStore for InMemoryTaskStore {
    async fn create_job(&self, request: CreateJob) -> Result<JobCreated, TaskStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();
        let identity = (request.region_code.clone(), request.revision);

        if let Some(&job_id) = state.by_identity.get(&identity) {
            let job = &state.jobs[&job_id];
            let existing: BTreeSet<(TileId, TaskType)> = run
                .task_ids
                .iter()
                .map(|id| {
                    let task = &state.tasks[id];
                    (task.tile, task.task_type)
                })
                .collect();
            let requested: BTreeSet<(TileId, TaskType)> =
                request.tasks.iter().map(task_key).collect();

            let same_policy =
                job.max_attempts == request.max_attempts && job.backoff == request.backoff;
            if existing != requested || !same_policy {
                return Err(TaskStoreError::JobConflict {
                    region_code: request.region_code,
                    revision: request.revision,
                });
            }

            return Ok(JobCreated {
                job_id,
                created: false,
                total_tasks: job.task_ids.len() as u32,
            });
        }

        let job_id = JobId::generate();
        let mut task_ids = Vec::with_capacity(request.tasks.len());
        for spec in &request.tasks {
            let task_id = TaskId::generate();
            state.tasks.insert(
                task_id,
                Task {
                    job_id,
                    tile: spec.tile,
                    task_type: spec.task_type,
                    state: TaskState::Pending,
                    attempts: 0,
                    claimable_at: now,
                    lease: None,
                    claimed_by: None,
                    claimed_at: None,
                    last_heartbeat_at: None,
                    last_failure: None,
                },
            );
            task_ids.push(task_id);
        }
        let total_tasks = task_ids.len() as u32;
        state.jobs.insert(
            job_id,
            Job {
                max_attempts: request.max_attempts,
                backoff: request.backoff,
                task_ids,
            },
        );
        state.by_identity.insert(identity, job_id);

        Ok(JobCreated {
            job_id,
            created: true,
            total_tasks,
        })
    }

    async fn claim(&self, _request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError> {
        unimplemented!("Task 5")
    }

    async fn heartbeat(&self, _lease: Lease) -> Result<(), TaskStoreError> {
        unimplemented!("Task 6")
    }

    async fn complete(&self, _lease: Lease) -> Result<(), TaskStoreError> {
        unimplemented!("Task 6")
    }

    async fn fail(&self, _request: FailRequest) -> Result<FailOutcome, TaskStoreError> {
        unimplemented!("Task 6")
    }

    async fn reap_expired(&self, _request: ReapRequest) -> Result<ReapOutcome, TaskStoreError> {
        unimplemented!("Task 7")
    }

    async fn job_status(&self, _job_id: JobId) -> Result<JobStatus, TaskStoreError> {
        unimplemented!("Task 7")
    }

    async fn throughput(&self, _job_id: JobId) -> Result<Throughput, TaskStoreError> {
        unimplemented!("Task 7")
    }
}
```

The `unimplemented!` stubs are deliberate and temporary: the trait must be fully implemented to compile at all, and Tasks 5-7 replace them. Each names the task that fills it so a stub surviving to the end is obvious.

- [ ] **Step 4: Wire the module**

Add to `oxo-tasks/src/lib.rs`:

```rust
pub mod memory;

pub use memory::InMemoryTaskStore;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --package oxo-tasks memory`
Expected: PASS, 6 tests.

- [ ] **Step 6: Run full verification and commit**

Run: `make verify`
Expected: PASS. If clippy objects to `as u32` on a `usize` length, prefer
`u32::try_from(len).unwrap_or(u32::MAX)` and say so in your report — a task
set larger than four billion is not a case worth a fallible signature.

```bash
git add oxo-tasks/
git commit -F - <<'EOF'
feat(tasks): add the in-memory store and job creation

create_job is idempotent on (region_code, revision), which is what makes
resubmitting a specification resume a job rather than duplicate weeks of
work. The task set is compared as a set, so reordering it is not a
different job — but a changed task set or failure policy under the same
identity is a conflict rather than a silent resume, since resuming would
run something other than what was asked for.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 5: In-memory adapter — `claim`

**Files:**
- Modify: `oxo-tasks/src/memory.rs`

**Interfaces:**
- Produces: `claim` replacing its stub. Picks the claimable task with the lowest `(claimable_at, TaskId)`, honours the task-type filter, increments `attempts`, mints a fresh `LeaseToken`, records worker and timestamps.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `oxo-tasks/src/memory.rs`:

```rust
    fn any(worker: &str) -> ClaimRequest {
        ClaimRequest {
            worker: worker.to_string(),
            task_types: None,
        }
    }

    #[tokio::test]
    async fn a_claim_hands_out_one_task_with_a_lease() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let claimed = store
            .claim(any("pod-1"))
            .await
            .expect("claim")
            .expect("a task was available");
        assert_eq!(claimed.attempt, 1);
    }

    #[tokio::test]
    async fn each_claim_mints_a_distinct_lease_token() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let first = store.claim(any("pod-1")).await.unwrap().unwrap();
        let second = store.claim(any("pod-2")).await.unwrap().unwrap();
        assert_ne!(first.task_id, second.task_id);
        assert_ne!(first.lease, second.lease);
    }

    #[tokio::test]
    async fn a_claimed_task_is_not_handed_out_again() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        store.claim(any("pod-1")).await.unwrap().unwrap();
        store.claim(any("pod-2")).await.unwrap().unwrap();
        let third = store.claim(any("pod-3")).await.unwrap();
        assert!(third.is_none(), "only two tasks exist, so the third claim finds none");
    }

    #[tokio::test]
    async fn an_empty_queue_is_not_an_error() {
        let store = store(clock());
        assert!(store.claim(any("pod-1")).await.expect("claim").is_none());
    }

    #[tokio::test]
    async fn a_task_type_filter_restricts_what_is_handed_out() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let claimed = store
            .claim(ClaimRequest {
                worker: "pod-1".to_string(),
                task_types: Some(vec![TaskType::Overlay]),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.task_type, TaskType::Overlay);

        let again = store
            .claim(ClaimRequest {
                worker: "pod-2".to_string(),
                task_types: Some(vec![TaskType::Overlay]),
            })
            .await
            .unwrap();
        assert!(again.is_none(), "only one overlay task exists");
    }

    #[tokio::test]
    async fn a_task_is_not_claimable_before_its_backoff_elapses() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.expect("create");

        // Claim and fail one task so it is requeued with backoff.
        let claimed = store.claim(any("pod-1")).await.unwrap().unwrap();
        store
            .fail(FailRequest {
                lease: Lease {
                    task_id: claimed.task_id,
                    token: claimed.lease,
                },
                reason: "Crash!".to_string(),
            })
            .await
            .expect("fail");

        // The other task is still claimable; take it out of the way.
        store.claim(any("pod-2")).await.unwrap().unwrap();

        assert!(
            store.claim(any("pod-3")).await.unwrap().is_none(),
            "the failed task is in backoff and must not be claimable yet"
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
        store.create_job(two_tile_job()).await.expect("create");

        // Fail the first task so it is requeued to a later claimable_at,
        // leaving the second task strictly older.
        let first = store.claim(any("pod-1")).await.unwrap().unwrap();
        store
            .fail(FailRequest {
                lease: Lease {
                    task_id: first.task_id,
                    token: first.lease,
                },
                reason: "transient".to_string(),
            })
            .await
            .expect("fail");

        let next = store.claim(any("pod-2")).await.unwrap().unwrap();
        assert_ne!(
            next.task_id, first.task_id,
            "the task still at its original claimable_at must come first"
        );
    }
```

Note that two of these tests call `fail`, which Task 6 implements. They will fail until then — that is expected and correct, because backoff is only observable through a failure. Implement `claim` in this task; the two backoff tests go green in Task 6. **Say so explicitly in your report rather than deleting them or stubbing `fail` to make them pass.**

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-tasks memory`
Expected: FAIL — the five claim tests panic at `unimplemented!("Task 5")`, and the two backoff tests panic at `unimplemented!("Task 6")`.

- [ ] **Step 3: Implement `claim`**

Replace the `claim` stub in `oxo-tasks/src/memory.rs`:

```rust
    async fn claim(&self, request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();

        let wanted = request.task_types.as_deref();
        let next = state
            .tasks
            .iter()
            .filter(|(_, task)| task.state == TaskState::Pending && task.claimable_at <= now)
            .filter(|(_, task)| wanted.map_or(true, |types| types.contains(&task.task_type)))
            .min_by_key(|(id, task)| (task.claimable_at, **id))
            .map(|(id, _)| *id);

        let Some(task_id) = next else {
            return Ok(None);
        };

        let token = LeaseToken::generate();
        let task = state
            .tasks
            .get_mut(&task_id)
            .expect("the id was just selected from this map");
        task.state = TaskState::Claimed;
        task.attempts += 1;
        task.lease = Some(token);
        task.claimed_by = Some(request.worker);
        task.claimed_at = Some(now);
        task.last_heartbeat_at = Some(now);

        Ok(Some(ClaimedTask {
            task_id,
            job_id: task.job_id,
            lease: token,
            tile: task.tile,
            task_type: task.task_type,
            attempt: task.attempts,
        }))
    }
```

`map_or(true, …)` rather than the newer `is_none_or`, which stabilised in
Rust 1.82 — after this workspace's declared 1.74 floor. Clippy honours
`rust-version` and so should not suggest the newer form; if it does anyway,
report that rather than raising the MSRV to satisfy a lint.
`wanted.map_or(true, |types| types.contains(&task.task_type))` and note the
substitution in your report.

- [ ] **Step 4: Run the tests to verify the claim tests pass**

Run: `cargo test --package oxo-tasks memory`
Expected: the five claim tests PASS; the two backoff tests still fail at `unimplemented!("Task 6")`. Report both counts.

- [ ] **Step 5: Commit**

```bash
git add oxo-tasks/
git commit -F - <<'EOF'
feat(tasks): hand out tasks with a lease, oldest claimable first

A claim takes the pending task with the lowest (claimable_at, id), mints a
fresh lease token and increments the attempt count. Attempts count starts
rather than failures, so a task reclaimed from a dead worker has already
consumed one — which is what stops a tile that kills its worker cycling
forever.

The optional task-type filter is how a worker expresses capacity until
there is a footprint model: one short on disk claims overlay work only.

Two backoff tests are present and still failing; backoff is only
observable through a failure, which Task 6 implements.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 6: In-memory adapter — `heartbeat`, `complete`, `fail`

**Files:**
- Modify: `oxo-tasks/src/memory.rs`

**Interfaces:**
- Produces: the three methods replacing their stubs. All three reject a stale lease with `TaskStoreError::LeaseLost`, an unclaimed task with `NotClaimed`, and an unknown task with `UnknownTask`. `fail` requeues with backoff while attempts remain and abandons at the limit.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `oxo-tasks/src/memory.rs`:

```rust
    async fn claim_one(store: &InMemoryTaskStore) -> ClaimedTask {
        store
            .claim(any("pod-1"))
            .await
            .expect("claim")
            .expect("a task was available")
    }

    fn lease_of(task: &ClaimedTask) -> Lease {
        Lease {
            task_id: task.task_id,
            token: task.lease,
        }
    }

    #[tokio::test]
    async fn a_heartbeat_on_a_held_lease_succeeds() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;
        store.heartbeat(lease_of(&task)).await.expect("still held");
    }

    #[tokio::test]
    async fn completing_a_held_task_succeeds_once_and_not_twice() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;

        store.complete(lease_of(&task)).await.expect("complete");

        let again = store.complete(lease_of(&task)).await.expect_err("already done");
        assert!(
            matches!(again, TaskStoreError::NotClaimed { .. }),
            "expected NotClaimed, got {again:?}"
        );
    }

    #[tokio::test]
    async fn a_stale_lease_is_rejected_by_all_three_reporting_calls() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;
        let stale = Lease {
            task_id: task.task_id,
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
                matches!(error, TaskStoreError::LeaseLost { .. }),
                "expected LeaseLost, got {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn reporting_on_an_unknown_task_says_so() {
        let store = store(clock());
        let nowhere = Lease {
            task_id: TaskId::generate(),
            token: LeaseToken::generate(),
        };
        let error = store.heartbeat(nowhere).await.expect_err("unknown");
        assert!(
            matches!(error, TaskStoreError::UnknownTask { .. }),
            "expected UnknownTask, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_failure_with_attempts_remaining_is_requeued_with_backoff() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;
        let at_failure = test_clock.now();

        let outcome = store
            .fail(FailRequest {
                lease: lease_of(&task),
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
    async fn a_task_is_abandoned_on_the_last_permitted_start() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.unwrap();

        // max_attempts is 3, so the third failure abandons.
        for expected_remaining in [2u32, 1] {
            let task = claim_one(&store).await;
            let outcome = store
                .fail(FailRequest {
                    lease: lease_of(&task),
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

Run: `cargo test --package oxo-tasks memory`
Expected: FAIL — the new tests and the two backoff tests from Task 5 panic at `unimplemented!("Task 6")`.

- [ ] **Step 3: Implement the three methods**

Add a helper above the `impl TaskStore` block in `oxo-tasks/src/memory.rs`:

```rust
impl State {
    /// Resolve a lease to a claimed task, rejecting the three ways it can be
    /// invalid. Shared by heartbeat, complete and fail so the checks cannot
    /// drift apart between them.
    fn claimed_mut(&mut self, lease: Lease) -> Result<&mut Task, TaskStoreError> {
        let task = self
            .tasks
            .get_mut(&lease.task_id)
            .ok_or(TaskStoreError::UnknownTask {
                task_id: lease.task_id,
            })?;
        if task.state != TaskState::Claimed {
            return Err(TaskStoreError::NotClaimed {
                task_id: lease.task_id,
            });
        }
        if task.lease != Some(lease.token) {
            return Err(TaskStoreError::LeaseLost {
                task_id: lease.task_id,
            });
        }
        Ok(task)
    }
}
```

Then replace the three stubs:

```rust
    async fn heartbeat(&self, lease: Lease) -> Result<(), TaskStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();
        let task = state.claimed_mut(lease)?;
        task.last_heartbeat_at = Some(now);
        Ok(())
    }

    async fn complete(&self, lease: Lease) -> Result<(), TaskStoreError> {
        let mut state = self.locked();
        let task = state.claimed_mut(lease)?;
        task.state = TaskState::Succeeded;
        task.lease = None;
        task.claimed_by = None;
        task.claimed_at = None;
        task.last_heartbeat_at = None;
        Ok(())
    }

    async fn fail(&self, request: FailRequest) -> Result<FailOutcome, TaskStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();

        // The job's policy is needed before the task is mutably borrowed.
        let job_id = state
            .tasks
            .get(&request.lease.task_id)
            .ok_or(TaskStoreError::UnknownTask {
                task_id: request.lease.task_id,
            })?
            .job_id;
        let job = state
            .jobs
            .get(&job_id)
            .ok_or(TaskStoreError::UnknownJob { job_id })?;
        let max_attempts = job.max_attempts;
        let backoff = job.backoff;

        let task = state.claimed_mut(request.lease)?;
        task.lease = None;
        task.claimed_by = None;
        task.claimed_at = None;
        task.last_heartbeat_at = None;
        task.last_failure = Some(request.reason);

        if task.attempts >= max_attempts {
            task.state = TaskState::Abandoned;
            return Ok(FailOutcome::Abandoned);
        }

        let step = chrono::Duration::from_std(backoff).unwrap_or(chrono::Duration::zero());
        let claimable_at = now + step;
        task.state = TaskState::Pending;
        task.claimable_at = claimable_at;

        Ok(FailOutcome::Requeued {
            claimable_at,
            attempts_remaining: max_attempts - task.attempts,
        })
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-tasks memory`
Expected: PASS — the six new tests and the two backoff tests from Task 5 all green. Report the total.

- [ ] **Step 5: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add oxo-tasks/
git commit -F - <<'EOF'
feat(tasks): report on a held task, and retire one that cannot succeed

heartbeat, complete and fail all resolve the caller's lease through one
shared check, so the three ways a lease can be invalid cannot drift apart
between them. A stale token is LeaseLost rather than being ignored: a
worker whose task was reclaimed must stop, not overwrite the state of
whoever holds it now.

fail requeues with the job's snapshotted backoff while starts remain and
abandons on the last permitted one. Because attempts increment at claim,
the budget is spent by starting, so abandonment lands exactly at
max_attempts starts however the task got there.

Also turns green the two backoff tests Task 5 left failing.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 7: In-memory adapter — `reap_expired`, `job_status`, `throughput`

**Files:**
- Modify: `oxo-tasks/src/memory.rs`
- Modify: `oxo-tasks/src/error.rs`

**Interfaces:**
- Produces: the three methods replacing their stubs, plus `TaskStoreError::EmptyJob`.

**A late addition, caught while designing `job_status`.** A job with no tasks would report `Complete`, because "every task succeeded" is vacuously true of none — a silently wrong answer of exactly the kind this project keeps finding. The specification layer already guarantees a non-empty tile set, so this is defence in depth, but a store that cheerfully declares nothing finished is a trap. `create_job` must reject an empty task set with a new `TaskStoreError::EmptyJob` variant. Add the variant and extend Task 2's `every_variant_renders_something_an_operator_can_act_on` table with a row for it.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `oxo-tasks/src/memory.rs`:

```rust
    fn reap(heartbeat_secs: u64, max_secs: u64) -> ReapRequest {
        ReapRequest {
            heartbeat_timeout: Duration::from_secs(heartbeat_secs),
            max_task_duration: Duration::from_secs(max_secs),
        }
    }

    #[tokio::test]
    async fn a_task_whose_heartbeat_lapses_is_reclaimed_and_takes_backoff() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;

        // Within the timeout: nothing is reclaimed.
        test_clock.advance(Duration::from_secs(60));
        let quiet = store.reap_expired(reap(90, 86_400)).await.expect("reap");
        assert_eq!(quiet, ReapOutcome::default());
        store.heartbeat(lease_of(&task)).await.expect("still held");

        // Past the timeout with no further heartbeat: reclaimed.
        test_clock.advance(Duration::from_secs(91));
        let reaped = store.reap_expired(reap(90, 86_400)).await.expect("reap");
        assert_eq!(reaped.requeued, 1);
        assert_eq!(reaped.abandoned, 0);

        // The old lease is now worthless.
        let error = store.heartbeat(lease_of(&task)).await.expect_err("reclaimed");
        assert!(
            matches!(error, TaskStoreError::LeaseLost { .. } | TaskStoreError::NotClaimed { .. }),
            "expected the old lease to be refused, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_reaped_task_is_not_instantly_reclaimable() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.unwrap();
        claim_one(&store).await;
        store.claim(any("pod-2")).await.unwrap().unwrap();

        test_clock.advance(Duration::from_secs(100));
        store.reap_expired(reap(90, 86_400)).await.expect("reap");

        assert!(
            store.claim(any("pod-3")).await.unwrap().is_none(),
            "a reaped task takes backoff; re-claiming it instantly would burn its budget in minutes"
        );
        test_clock.advance(Duration::from_secs(60));
        assert!(store.claim(any("pod-3")).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_worker_that_heartbeats_forever_is_still_cut_off_by_the_backstop() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;

        // Heartbeat diligently for well past the maximum duration.
        for _ in 0..10 {
            test_clock.advance(Duration::from_secs(60));
            let _ = store.heartbeat(lease_of(&task)).await;
        }

        let reaped = store.reap_expired(reap(90, 300)).await.expect("reap");
        assert_eq!(
            reaped.requeued, 1,
            "a wedged-but-alive worker must not hold a task indefinitely"
        );
    }

    #[tokio::test]
    async fn a_reap_consumes_an_attempt_and_can_abandon() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        let mut job = two_tile_job();
        job.max_attempts = 1;
        store.create_job(job).await.unwrap();

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
        let job = store.create_job(two_tile_job()).await.unwrap();

        assert_eq!(
            store.job_status(job.job_id).await.unwrap(),
            JobStatus::InProgress {
                pending: 2,
                claimed: 0,
                succeeded: 0,
                abandoned: 0
            }
        );

        for _ in 0..2 {
            let task = claim_one(&store).await;
            store.complete(lease_of(&task)).await.unwrap();
        }

        assert_eq!(store.job_status(job.job_id).await.unwrap(), JobStatus::Complete);
    }

    #[tokio::test]
    async fn an_abandoned_task_is_visible_while_work_continues_then_fails_the_run() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        let mut spec = two_tile_job();
        spec.max_attempts = 1;
        let job = store.create_job(spec).await.unwrap();

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
            store.job_status(job.job_id).await.unwrap(),
            JobStatus::InProgress {
                pending: 1,
                claimed: 0,
                succeeded: 0,
                abandoned: 1
            }
        );

        let other = claim_one(&store).await;
        store.complete(lease_of(&other)).await.unwrap();

        assert_eq!(
            store.job_status(job.job_id).await.unwrap(),
            JobStatus::Failed { abandoned: 1 }
        );
    }

    #[tokio::test]
    async fn the_gate_rejects_a_run_it_does_not_know() {
        let store = store(clock());
        let error = store
            .job_status(JobId::generate())
            .await
            .expect_err("unknown job");
        assert!(
            matches!(error, TaskStoreError::UnknownJob { .. }),
            "expected UnknownJob, got {error:?}"
        );
    }

    #[tokio::test]
    async fn throughput_separates_pending_from_claimable_now() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        let job = store.create_job(two_tile_job()).await.unwrap();

        let task = claim_one(&store).await;
        store
            .fail(FailRequest {
                lease: lease_of(&task),
                reason: "Crash!".to_string(),
            })
            .await
            .unwrap();

        let snapshot = store.throughput(job.job_id).await.unwrap();
        assert_eq!(snapshot.pending, 2, "both tasks are pending");
        assert_eq!(
            snapshot.claimable_now, 1,
            "one is in backoff, so only one can be claimed right now"
        );

        test_clock.advance(Duration::from_secs(60));
        let later = store.throughput(job.job_id).await.unwrap();
        assert_eq!(later.claimable_now, 2);
    }

    #[tokio::test]
    async fn a_run_with_no_tasks_is_refused_rather_than_declared_complete() {
        let store = store(clock());
        let mut empty = two_tile_job();
        empty.tasks.clear();
        let error = store.create_job(empty).await.expect_err("empty run");
        assert!(
            matches!(error, TaskStoreError::EmptyJob { .. }),
            "expected EmptyJob, got {error:?}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-tasks memory`
Expected: FAIL — the new tests panic at `unimplemented!("Task 7")`, and the empty-run test fails to compile because `TaskStoreError::EmptyJob` does not exist.

- [ ] **Step 3: Add the `EmptyJob` variant**

Add to `TaskStoreError` in `oxo-tasks/src/error.rs`:

```rust
    /// A job must contain at least one task. A job of none would report
    /// `Complete` vacuously, which is a silently wrong answer.
    #[error("a job for region {region_code} revision {revision} must contain at least one task")]
    EmptyJob { region_code: String, revision: u32 },
```

Extend the `every_variant_renders_something_an_operator_can_act_on` table in that file's tests with:

```rust
            (
                TaskStoreError::EmptyJob {
                    region_code: "NA".to_string(),
                    revision: 1,
                },
                "at least one task",
            ),
```

Then add the guard as the first thing `create_job` does, before the identity lookup:

```rust
        if request.tasks.is_empty() {
            return Err(TaskStoreError::EmptyJob {
                region_code: request.region_code,
                revision: request.revision,
            });
        }
```

- [ ] **Step 4: Implement the three methods**

Add a helper to the `impl State` block:

```rust
impl State {
    /// Count tasks per state for one job. Shared by the gate and the
    /// throughput snapshot so the two cannot disagree.
    fn tally(&self, job_id: JobId, now: DateTime<Utc>) -> Result<Tally, TaskStoreError> {
        let job = self
            .jobs
            .get(&job_id)
            .ok_or(TaskStoreError::UnknownJob { job_id })?;
        let mut tally = Tally::default();
        for id in &job.task_ids {
            let task = &self.tasks[id];
            match task.state {
                TaskState::Pending => {
                    tally.pending += 1;
                    if task.claimable_at <= now {
                        tally.claimable_now += 1;
                    }
                }
                TaskState::Claimed => tally.claimed += 1,
                TaskState::Succeeded => tally.succeeded += 1,
                TaskState::Abandoned => tally.abandoned += 1,
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
    async fn reap_expired(&self, request: ReapRequest) -> Result<ReapOutcome, TaskStoreError> {
        let now = self.clock.now();
        let heartbeat_timeout = chrono::Duration::from_std(request.heartbeat_timeout)
            .unwrap_or(chrono::Duration::zero());
        let max_duration = chrono::Duration::from_std(request.max_task_duration)
            .unwrap_or(chrono::Duration::zero());
        let mut state = self.locked();

        // Policies are read before any task is mutably borrowed.
        let policies: BTreeMap<JobId, (u32, Duration)> = state
            .jobs
            .iter()
            .map(|(id, run)| (*id, (job.max_attempts, job.backoff)))
            .collect();

        let expired: Vec<TaskId> = state
            .tasks
            .iter()
            .filter(|(_, task)| task.state == TaskState::Claimed)
            .filter(|(_, task)| {
                let heartbeat_lapsed = task
                    .last_heartbeat_at
                    .is_some_and(|last| now - last > heartbeat_timeout);
                let held_too_long = task
                    .claimed_at
                    .is_some_and(|since| now - since > max_duration);
                heartbeat_lapsed || held_too_long
            })
            .map(|(id, _)| *id)
            .collect();

        let mut outcome = ReapOutcome::default();
        for task_id in expired {
            let (max_attempts, backoff) = policies[&state.tasks[&task_id].job_id];
            let task = state.tasks.get_mut(&task_id).expect("just selected");
            task.lease = None;
            task.claimed_by = None;
            task.claimed_at = None;
            task.last_heartbeat_at = None;

            if task.attempts >= max_attempts {
                task.state = TaskState::Abandoned;
                outcome.abandoned += 1;
            } else {
                let step =
                    chrono::Duration::from_std(backoff).unwrap_or(chrono::Duration::zero());
                task.state = TaskState::Pending;
                task.claimable_at = now + step;
                outcome.requeued += 1;
            }
        }

        Ok(outcome)
    }

    async fn job_status(&self, job_id: JobId) -> Result<JobStatus, TaskStoreError> {
        let now = self.clock.now();
        let state = self.locked();
        let tally = state.tally(job_id, now)?;

        if tally.pending == 0 && tally.claimed == 0 {
            return Ok(if tally.abandoned == 0 {
                JobStatus::Complete
            } else {
                JobStatus::Failed {
                    abandoned: tally.abandoned,
                }
            });
        }

        Ok(JobStatus::InProgress {
            pending: tally.pending,
            claimed: tally.claimed,
            succeeded: tally.succeeded,
            abandoned: tally.abandoned,
        })
    }

    async fn throughput(&self, job_id: JobId) -> Result<Throughput, TaskStoreError> {
        let now = self.clock.now();
        let state = self.locked();
        let tally = state.tally(job_id, now)?;
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

Run: `cargo test --package oxo-tasks`
Expected: PASS. Every `unimplemented!` is now gone — confirm with
`grep -c 'unimplemented!' oxo-tasks/src/memory.rs`, which must print `0`.

- [ ] **Step 6: Run full verification and commit**

Run: `make verify`
Expected: PASS.

```bash
git add oxo-tasks/
git commit -F - <<'EOF'
feat(tasks): reclaim dead work, and answer whether a region is finished

A reap reclaims a claimed task on either of two conditions: its heartbeat
lapsed, or it has been held past the maximum duration regardless of
heartbeats — the latter covering a worker that is wedged but alive and
would otherwise hold a task forever. Either way it takes the same backoff
as a reported failure, so a tile that kills its worker cannot be
re-claimed instantly and spend its whole budget in minutes. Because
attempts increment at claim, a reap with the budget already spent
abandons rather than requeueing, which is what stops such a tile cycling.

The gate and the throughput snapshot share one tally, so they cannot
disagree. Abandoned tasks are reported during InProgress as well as in
Failed: a job with an abandoned tile is already unachievable, and an
operator should learn that in minutes rather than after a fortnight of
other tiles finishing.

Also refuses a job with no tasks. Planning found that one would report
Complete vacuously, which is a silently wrong answer of exactly the kind
this project keeps uncovering.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 8: The conformance suite

**Files:**
- Create: `oxo-tasks/src/conformance.rs`
- Create: `oxo-tasks/tests/conformance_memory.rs`
- Modify: `oxo-tasks/src/lib.rs`
- Modify: `oxo-tasks/Cargo.toml`

**Interfaces:**
- Produces: a `conformance` feature; `Subject { store, clock }`; the `Fixture` trait with `async fn fresh(&self) -> Subject`; one `pub async fn` per invariant; and the `conformance_suite!` macro, which expands to one `#[tokio::test]` per case.
- Consumed by: `oxo-tasks/tests/conformance_memory.rs` now, and `oxo-tasks-postgres` in Task 12.

**Why a macro.** Both adapters must run every case, and adding a case later must not mean editing two crates. The macro keeps the case list in exactly one place — here — while giving each case its own test name in the output, so a failure names the invariant that broke rather than "the suite".

- [ ] **Step 1: Add the feature**

Modify `oxo-tasks/Cargo.toml`:

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

Create `oxo-tasks/src/conformance.rs`:

```rust
//! A contract every [`TaskStore`] adapter must satisfy.
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
use crate::error::TaskStoreError;
use crate::task::TaskType;
use crate::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, TaskSpec, Lease, ReapRequest,
    JobStatus,
};
use crate::store::TaskStore;

/// A freshly-made, empty store and the clock it reads.
pub struct Subject {
    pub store: Box<dyn TaskStore>,
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

/// Two tasks for one tile: the ortho build and its overlay.
pub fn two_task_task() -> CreateJob {
    CreateJob {
        region_code: "NA".to_string(),
        revision: 1,
        max_attempts: 3,
        backoff: Duration::from_secs(60),
        tasks: vec![
            TaskSpec {
                tile: tile(50, -2),
                task_type: TaskType::Ortho,
            },
            TaskSpec {
                tile: tile(50, -2),
                task_type: TaskType::Overlay,
            },
        ],
    }
}

fn any_task(worker: &str) -> ClaimRequest {
    ClaimRequest {
        worker: worker.to_string(),
        task_types: None,
    }
}

fn lease_of(task: &ClaimedTask) -> Lease {
    Lease {
        task_id: task.task_id,
        token: task.lease,
    }
}

// ─── cases ───────────────────────────────────────────────────────────────

pub async fn creating_a_run_twice_resumes_it(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let first = subject.store.create_job(two_task_task()).await.expect("create");
    let second = subject.store.create_job(two_task_task()).await.expect("resume");
    assert!(first.created);
    assert!(!second.created);
    assert_eq!(first.job_id, second.job_id);
    assert_eq!(second.total_tasks, 2);
}

pub async fn a_changed_task_set_under_one_identity_conflicts(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject.store.create_job(two_task_task()).await.expect("create");
    let mut altered = two_task_task();
    altered.tasks.push(TaskSpec {
        tile: tile(51, -2),
        task_type: TaskType::Ortho,
    });
    let error = subject
        .store
        .create_job(altered)
        .await
        .expect_err("should conflict");
    assert!(
        matches!(error, TaskStoreError::JobConflict { .. }),
        "expected JobConflict, got {error:?}"
    );
}

pub async fn a_run_with_no_tasks_is_refused(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut empty = two_task_task();
    empty.tasks.clear();
    let error = subject
        .store
        .create_job(empty)
        .await
        .expect_err("should refuse");
    assert!(
        matches!(error, TaskStoreError::EmptyJob { .. }),
        "expected EmptyJob, got {error:?}"
    );
}

pub async fn every_task_is_handed_out_exactly_once(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject.store.create_job(two_task_task()).await.expect("create");

    let mut seen = Vec::new();
    while let Some(task) = subject
        .store
        .claim(any_task("pod"))
        .await
        .expect("claim")
    {
        seen.push(task.task_id);
    }
    seen.sort();
    let unique = {
        let mut copy = seen.clone();
        copy.dedup();
        copy
    };
    assert_eq!(seen.len(), 2, "both tasks were handed out");
    assert_eq!(seen, unique, "no task was handed out twice");
}

pub async fn an_empty_queue_yields_none_not_an_error(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    assert!(subject
        .store
        .claim(any_task("pod"))
        .await
        .expect("claim")
        .is_none());
}

pub async fn a_task_type_filter_is_honoured(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject.store.create_job(two_task_task()).await.expect("create");
    let claimed = subject
        .store
        .claim(ClaimRequest {
            worker: "pod".to_string(),
            task_types: Some(vec![TaskType::Overlay]),
        })
        .await
        .expect("claim")
        .expect("an overlay task exists");
    assert_eq!(claimed.task_type, TaskType::Overlay);
}

pub async fn a_stale_lease_is_refused_by_every_reporting_call(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject.store.create_job(two_task_task()).await.expect("create");
    let task = subject
        .store
        .claim(any_task("pod"))
        .await
        .expect("claim")
        .expect("a task");
    let stale = Lease {
        task_id: task.task_id,
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
            matches!(error, TaskStoreError::LeaseLost { .. }),
            "expected LeaseLost, got {error:?}"
        );
    }
}

pub async fn a_failure_is_requeued_until_the_budget_is_spent(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut job = two_task_task();
    job.max_attempts = 2;
    subject.store.create_job(job).await.expect("create");

    let first = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");
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
        if let Some(task) = subject.store.claim(any_task("pod")).await.unwrap() {
            if task.task_id == first.task_id {
                break task;
            }
        } else {
            panic!("the requeued task never became claimable");
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

pub async fn a_lapsed_heartbeat_reclaims_the_task_and_spends_a_start(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut job = two_task_task();
    job.max_attempts = 1;
    subject.store.create_job(job).await.expect("create");

    subject.store.claim(any_task("pod")).await.unwrap().expect("a task");
    subject.clock.advance(Duration::from_secs(100));

    let reaped = subject
        .store
        .reap_expired(ReapRequest {
            heartbeat_timeout: Duration::from_secs(90),
            max_task_duration: Duration::from_secs(86_400),
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
    subject.store.create_job(two_task_task()).await.expect("create");
    let task = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");

    for _ in 0..10 {
        subject.clock.advance(Duration::from_secs(30));
        let _ = subject.store.heartbeat(lease_of(&task)).await;
    }

    let reaped = subject
        .store
        .reap_expired(ReapRequest {
            heartbeat_timeout: Duration::from_secs(90),
            max_task_duration: Duration::from_secs(120),
        })
        .await
        .expect("reap");
    assert_eq!(reaped.requeued, 1);
}

pub async fn the_gate_moves_from_in_progress_to_complete(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let job = subject.store.create_job(two_task_task()).await.expect("create");
    assert!(matches!(
        subject.store.job_status(job.job_id).await.expect("status"),
        JobStatus::InProgress { pending: 2, .. }
    ));

    while let Some(task) = subject.store.claim(any_task("pod")).await.unwrap() {
        subject.store.complete(lease_of(&task)).await.expect("complete");
    }

    assert_eq!(
        subject.store.job_status(job.job_id).await.expect("status"),
        JobStatus::Complete
    );
}

pub async fn an_abandoned_task_is_visible_before_it_fails_the_run(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut spec = two_task_task();
    spec.max_attempts = 1;
    let job = subject.store.create_job(spec).await.expect("create");

    let doomed = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");
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
            subject.store.job_status(job.job_id).await.expect("status"),
            JobStatus::InProgress { abandoned: 1, .. }
        ),
        "an unachievable run must be visible while other work continues"
    );

    let other = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");
    subject.store.complete(lease_of(&other)).await.expect("complete");

    assert_eq!(
        subject.store.job_status(job.job_id).await.expect("status"),
        JobStatus::Failed { abandoned: 1 }
    );
}

pub async fn an_unknown_run_is_refused_by_the_gate(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let error = subject
        .store
        .job_status(crate::ids::JobId::generate())
        .await
        .expect_err("unknown job");
    assert!(
        matches!(error, TaskStoreError::UnknownJob { .. }),
        "expected UnknownJob, got {error:?}"
    );
}

pub async fn throughput_separates_pending_from_claimable_now(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let job = subject.store.create_job(two_task_task()).await.expect("create");
    let task = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");
    subject
        .store
        .fail(FailRequest {
            lease: lease_of(&task),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");

    let snapshot = subject.store.throughput(job.job_id).await.expect("throughput");
    assert_eq!(snapshot.pending, 2);
    assert_eq!(snapshot.claimable_now, 1);

    subject.clock.advance(Duration::from_secs(60));
    let later = subject.store.throughput(job.job_id).await.expect("throughput");
    assert_eq!(later.claimable_now, 2);
}

/// The invariant that matters most under a real database: concurrent
/// claimants must between them see each task exactly once.
pub async fn concurrent_claims_hand_each_task_out_exactly_once(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut job = two_task_task();
    job.tasks = (0..24)
        .map(|n| TaskSpec {
            tile: tile(50, -24 + n),
            task_type: TaskType::Ortho,
        })
        .collect();
    let created = subject.store.create_job(job).await.expect("create");
    assert_eq!(created.total_tasks, 24);

    let store: Arc<dyn TaskStore> = Arc::from(subject.store);
    let mut workers = Vec::new();
    for worker in 0..8 {
        let store = Arc::clone(&store);
        workers.push(tokio::spawn(async move {
            let mut mine = Vec::new();
            while let Some(task) = store
                .claim(ClaimRequest {
                    worker: format!("pod-{worker}"),
                    task_types: None,
                })
                .await
                .expect("claim")
            {
                mine.push(task.task_id);
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

    assert_eq!(all.len(), 24, "every task was claimed");
    assert_eq!(all, unique, "no task was claimed twice");
}
```

- [ ] **Step 3: Write the macro**

Append to `oxo-tasks/src/conformance.rs`:

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
        $crate::conformance_case!($fixture, a_changed_task_set_under_one_identity_conflicts);
        $crate::conformance_case!($fixture, a_run_with_no_tasks_is_refused);
        $crate::conformance_case!($fixture, every_task_is_handed_out_exactly_once);
        $crate::conformance_case!($fixture, an_empty_queue_yields_none_not_an_error);
        $crate::conformance_case!($fixture, a_task_type_filter_is_honoured);
        $crate::conformance_case!($fixture, a_stale_lease_is_refused_by_every_reporting_call);
        $crate::conformance_case!($fixture, a_failure_is_requeued_until_the_budget_is_spent);
        $crate::conformance_case!($fixture, a_lapsed_heartbeat_reclaims_the_task_and_spends_a_start);
        $crate::conformance_case!($fixture, a_diligent_but_wedged_worker_is_cut_off_by_the_backstop);
        $crate::conformance_case!($fixture, the_gate_moves_from_in_progress_to_complete);
        $crate::conformance_case!($fixture, an_abandoned_task_is_visible_before_it_fails_the_run);
        $crate::conformance_case!($fixture, an_unknown_run_is_refused_by_the_gate);
        $crate::conformance_case!($fixture, throughput_separates_pending_from_claimable_now);
        $crate::conformance_case!($fixture, concurrent_claims_hand_each_task_out_exactly_once);
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

Add to `oxo-tasks/src/lib.rs`:

```rust
#[cfg(feature = "conformance")]
pub mod conformance;
```

Create `oxo-tasks/tests/conformance_memory.rs`:

```rust
//! The conformance suite, run against the in-memory adapter.
//!
//! The same cases run against `oxo-tasks-postgres`. If one adapter passes a
//! case the other fails, the two have diverged — which is the whole reason
//! this suite exists.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use oxo_tasks::clock::TestClock;
use oxo_tasks::conformance::{Fixture, Subject};
use oxo_tasks::InMemoryTaskStore;

struct Memory;

#[async_trait]
impl Fixture for Memory {
    async fn fresh(&self) -> Subject {
        let clock = Arc::new(TestClock::new(
            chrono::Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        ));
        Subject {
            store: Box::new(InMemoryTaskStore::new(clock.clone())),
            clock,
        }
    }
}

oxo_tasks::conformance_suite!(Memory);
```

- [ ] **Step 5: Run the suite**

Run: `cargo test --package oxo-tasks --features conformance --test conformance_memory`
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
git add oxo-tasks/ Makefile
git commit -F - <<'EOF'
test(tasks): add the conformance suite both adapters must satisfy

Fifteen cases asserting invariants rather than implementation: a task is
handed out exactly once, a stale lease is refused by every reporting
call, a reap spends a start, a wedged worker is cut off by the backstop,
the gate moves through its three shapes, and concurrent claimants between
them see each task exactly once.

The case list lives in one macro, so adding a case covers every adapter
without touching their crates, while each case still gets its own test
name — a failure names the invariant that broke rather than "the suite".

make verify now passes --all-features so the suite is part of default
verification rather than something only a separate command reaches.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 9: `oxo-tasks-postgres` — crate, schema, and the disposable database

**Files:**
- Modify: `Cargo.toml`
- Modify: `Makefile`
- Create: `oxo-tasks-postgres/Cargo.toml`
- Create: `oxo-tasks-postgres/migrations/0001_tasks.sql`
- Create: `oxo-tasks-postgres/src/lib.rs`

**Interfaces:**
- Produces: `PostgresTaskStore::new(pool: PgPool, clock: Arc<dyn Clock>)`, `run_migrations(&PgPool) -> Result<(), sqlx::Error>`, and the schema. Port methods are stubbed with `unimplemented!()` naming the task that fills them (10, 11, 12).
- Make targets: `pg-up`, `pg-down`, `verify-db`.

**Task type and state are stored as `text` with a `CHECK` constraint, not as PostgreSQL enums.** A PostgreSQL enum would need `#[derive(sqlx::Type)]` on `TaskType`, which would drag `sqlx` into `oxo-tasks` and break the dependency boundary that is the whole point of the split. `text` plus `CHECK` keeps the constraint in the database while the conversion uses the exact string forms Task 2 built for precisely this.

- [ ] **Step 1: Add the crate and the test exclusion**

Add `"oxo-tasks-postgres"` to `[workspace] members` and add to `[workspace.dependencies]`:

```toml
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "tls-none", "postgres", "uuid", "chrono", "macros", "migrate"] }
```

Create `oxo-tasks-postgres/Cargo.toml`:

```toml
[package]
name = "oxo-tasks-postgres"
description = "PostgreSQL adapter for the OXO task store"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
async-trait = { workspace = true }
chrono = { workspace = true }
oxo-tasks = { path = "../oxo-tasks" }
oxo-spec = { path = "../oxo-spec" }
sqlx = { workspace = true }
uuid = { workspace = true }

[dev-dependencies]
oxo-tasks = { path = "../oxo-tasks", features = ["conformance"] }
tokio = { workspace = true }

[[test]]
name = "conformance_postgres"
```

Modify the `test` and `test-strict` targets in the `Makefile` to exclude this package by name:

```makefile
.PHONY: test
test: ## Run all tests except those needing a database (see verify-db)
	$(CARGO) test --workspace --exclude oxo-tasks-postgres --all-targets --all-features

.PHONY: test-strict
test-strict: ## Run all tests except database ones, warnings as errors
	RUSTFLAGS="-D warnings" $(CARGO) test --workspace --exclude oxo-tasks-postgres --all-targets --all-features
```

**Excluded by name, never skipped at runtime.** A runtime skip is how a suite reports green while testing nothing — the trap sub-project 1 hit, where a renamed Gherkin step became a silent skip at exit code 0. Exclusion by name means the absence of database coverage is visible in which target you ran. `make lint` keeps `--workspace` with no exclusion, so this crate is still compiled and linted by default verification.

- [ ] **Step 2: Add the disposable-database targets**

Append to the `Makefile`:

```makefile
PG_TEST_CONTAINER ?= oxo-tasks-test-pg
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
	DATABASE_URL=$(PG_TEST_URL) $(CARGO) test --package oxo-tasks-postgres --all-features; \
	status=$$?; $(MAKE) pg-down; exit $$status
```

The teardown runs whether the tests passed or failed, so a failure does not
leave a container behind. If `podman` is unavailable the target fails
loudly, which is correct — it cannot do its task.

- [ ] **Step 3: Write the schema**

Create `oxo-tasks-postgres/migrations/0001_tasks.sql`:

```sql
-- Task type and state are text with a CHECK rather than PostgreSQL enums.
-- An enum would require sqlx::Type on the Rust enums, dragging sqlx into
-- oxo-tasks and breaking the dependency boundary the crate split exists to
-- maintain. The accepted labels match TaskType::as_str and TaskState::as_str
-- exactly, and from_str_exact does no case folding, so the two cannot drift
-- apart silently.

CREATE TABLE jobs (
    id           uuid        PRIMARY KEY,
    region_code  text        NOT NULL,
    revision     integer     NOT NULL,
    -- Snapshotted from the specification's failure policy, so editing a
    -- specification cannot change the policy of a job already in flight.
    max_attempts integer     NOT NULL CHECK (max_attempts >= 1),
    backoff_secs bigint      NOT NULL CHECK (backoff_secs >= 0),
    created_at   timestamptz NOT NULL,
    UNIQUE (region_code, revision)
);

CREATE TABLE tasks (
    id                uuid        PRIMARY KEY,
    job_id            uuid        NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    tile              text        NOT NULL,
    task_type          text        NOT NULL CHECK (task_type IN ('ortho', 'overlay')),
    state             text        NOT NULL CHECK (state IN ('pending', 'claimed', 'succeeded', 'abandoned')),
    -- Counts starts, not failures: incremented at claim.
    attempts          integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claimable_at      timestamptz NOT NULL,
    lease_token       uuid,
    claimed_by        text,
    claimed_at        timestamptz,
    last_heartbeat_at timestamptz,
    last_failure      text,
    UNIQUE (job_id, tile, task_type),
    -- A claimed task holds a lease and a claim time; nothing else does.
    CONSTRAINT lease_matches_state CHECK (
        (state = 'claimed') = (lease_token IS NOT NULL)
        AND (state = 'claimed') = (claimed_at IS NOT NULL)
    )
);

-- The claim query's hot path.
CREATE INDEX tasks_claimable ON tasks (claimable_at, id) WHERE state = 'pending';
-- The reaper's hot path.
CREATE INDEX tasks_claimed ON tasks (last_heartbeat_at) WHERE state = 'claimed';
```

The `lease_matches_state` constraint is deliberate: it makes "a task holds a lease if and only if it is claimed" a property the database enforces, so an adapter bug that leaves a dangling lease is a write failure rather than silent corruption.

- [ ] **Step 4: Write the crate skeleton**

Create `oxo-tasks-postgres/src/lib.rs`:

```rust
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
use oxo_tasks::ids::JobId;
use oxo_tasks::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, Lease, ReapOutcome, ReapRequest,
    JobCreated, JobStatus, Throughput,
};
use oxo_tasks::store::TaskStore;
use sqlx::PgPool;

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
    async fn create_job(&self, _request: CreateJob) -> Result<JobCreated, TaskStoreError> {
        unimplemented!("Task 10")
    }
    async fn claim(&self, _request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError> {
        unimplemented!("Task 10")
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
```

- [ ] **Step 5: Write the failing migration test**

Create `oxo-tasks-postgres/tests/conformance_postgres.rs`:

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
    assert!(tables.iter().any(|t| t == "runs"), "{tables:?}");
    assert!(tables.iter().any(|t| t == "tasks"), "{tables:?}");
}
```

- [ ] **Step 6: Run it and verify it fails for the right reason**

Run: `make verify-db`
Expected: the container starts, then FAIL — the migration directory is read but the crate does not compile, or the assertion fails because the schema is absent. Capture the real output. Then make it pass by correcting whatever the failure names.

Run: `make verify`
Expected: PASS, and it must **not** attempt the database tests. Confirm by reading the output: no `conformance_postgres` target should appear.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock Makefile oxo-tasks-postgres/
git commit -F - <<'EOF'
feat(tasks-pg): add the PostgreSQL adapter crate, schema and test harness

Task type and state are text with a CHECK constraint rather than PostgreSQL
enums: an enum would need sqlx::Type on the Rust enums, dragging sqlx into
oxo-tasks and breaking the dependency boundary the crate split exists to
maintain. A further constraint makes "a task holds a lease if and only if
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

### Task 10: PostgreSQL adapter — `create_job` and `claim`

**Files:**
- Modify: `oxo-tasks-postgres/src/lib.rs`
- Modify: `oxo-tasks-postgres/tests/conformance_postgres.rs`

**Interfaces:**
- Produces: `create_job` and `claim` replacing their stubs, with behaviour identical to the in-memory adapter.

- [ ] **Step 1: Write the fixture and enable the first conformance cases**

Add to `oxo-tasks-postgres/tests/conformance_postgres.rs`:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use oxo_tasks::clock::TestClock;
use oxo_tasks::conformance::{Fixture, Subject};
use oxo_tasks_postgres::PostgresTaskStore;
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
```

- [ ] **Step 2: Run the suite to verify it fails**

Run: `make verify-db`
Expected: FAIL — every conformance case panics at `unimplemented!("Task 10")` or later. Capture the output. The migration test should still pass.

- [ ] **Step 3: Implement `create_job`**

Replace the stub in `oxo-tasks-postgres/src/lib.rs`:

```rust
    async fn create_job(&self, request: CreateJob) -> Result<JobCreated, TaskStoreError> {
        if request.tasks.is_empty() {
            return Err(TaskStoreError::EmptyJob {
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
            "SELECT id, max_attempts, backoff_secs FROM jobs \
             WHERE region_code = $1 AND revision = $2 FOR UPDATE",
        )
        .bind(&request.region_code)
        .bind(revision)
        .fetch_optional(&mut *tx)
        .await
        .map_err(adapter)?;

        if let Some((run_uuid, existing_attempts, existing_backoff)) = existing {
            let job_id = JobId::from_uuid(run_uuid);
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT tile, task_type FROM tasks WHERE job_id = $1")
                    .bind(run_uuid)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(adapter)?;

            let mut existing_set: Vec<(String, String)> = rows;
            existing_set.sort();
            let mut requested: Vec<(String, String)> = request
                .tasks
                .iter()
                .map(|spec| (spec.tile.to_string(), spec.task_type.as_str().to_string()))
                .collect();
            requested.sort();
            requested.dedup();

            let same_policy =
                existing_attempts == max_attempts && existing_backoff == backoff_secs;
            if existing_set != requested || !same_policy {
                return Err(TaskStoreError::JobConflict {
                    region_code: request.region_code,
                    revision: request.revision,
                });
            }

            tx.commit().await.map_err(adapter)?;
            let total_tasks = u32::try_from(existing_set.len()).unwrap_or(u32::MAX);
            return Ok(JobCreated {
                job_id,
                created: false,
                total_tasks,
            });
        }

        let run_uuid = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO jobs (id, region_code, revision, max_attempts, backoff_secs, created_at) \
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

        for spec in &request.tasks {
            sqlx::query(
                "INSERT INTO tasks (id, job_id, tile, task_type, state, attempts, claimable_at) \
                 VALUES ($1, $2, $3, $4, 'pending', 0, $5) \
                 ON CONFLICT (job_id, tile, task_type) DO NOTHING",
            )
            .bind(Uuid::new_v4())
            .bind(run_uuid)
            .bind(spec.tile.to_string())
            .bind(spec.task_type.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(adapter)?;
        }

        let total_tasks: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE job_id = $1")
            .bind(run_uuid)
            .fetch_one(&mut *tx)
            .await
            .map_err(adapter)?;

        tx.commit().await.map_err(adapter)?;

        Ok(JobCreated {
            job_id: JobId::from_uuid(run_uuid),
            created: true,
            total_tasks: u32::try_from(total_tasks).unwrap_or(u32::MAX),
        })
    }
```

The transaction here is internal to one port call, which the design permits — what it forbids is a transaction *spanning* calls.

- [ ] **Step 4: Implement `claim`**

Replace the stub:

```rust
    async fn claim(&self, request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError> {
        let now = self.clock.now();
        let token = LeaseToken::generate();
        let wanted: Option<Vec<String>> = request.task_types.as_ref().map(|types| {
            types
                .iter()
                .map(|task_type| task_type.as_str().to_string())
                .collect()
        });

        let row: Option<(Uuid, Uuid, String, String, i32)> = sqlx::query_as(
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

        let Some((task_uuid, run_uuid, tile, task_type, attempts)) = row else {
            return Ok(None);
        };

        Ok(Some(ClaimedTask {
            task_id: TaskId::from_uuid(task_uuid),
            job_id: JobId::from_uuid(run_uuid),
            lease: token,
            tile: tile.parse().map_err(|error| {
                TaskStoreError::Adapter(format!("stored tile {tile:?} is not a valid identifier: {error}"))
            })?,
            task_type: TaskType::from_str_exact(&task_type).ok_or_else(|| {
                TaskStoreError::Adapter(format!("stored task type {task_type:?} is not recognised"))
            })?,
            attempt: u32::try_from(attempts).unwrap_or(u32::MAX),
        }))
    }
```

Add the imports this needs: `oxo_tasks::ids::{TaskId, LeaseToken}`, `oxo_tasks::task::TaskType`, `uuid::Uuid`.

Note the two `Adapter` errors on conversion. They should be unreachable — the `CHECK` constraint and the specification's validation both prevent them — but a store that silently mangled a tile identifier would be far worse than one that says it found something it could not read.

- [ ] **Step 5: Run the suite**

Run: `make verify-db`
Expected: the creation, claim, filter, empty-queue and concurrency cases PASS; the rest still fail at later `unimplemented!`. **Report which cases pass and which do not** — the concurrency case passing here is the first real evidence `SKIP LOCKED` behaves as intended.

- [ ] **Step 6: Commit**

```bash
git add oxo-tasks-postgres/
git commit -F - <<'EOF'
feat(tasks-pg): create runs idempotently and claim with SKIP LOCKED

create_job locks the (region_code, revision) identity before deciding, so
two concurrent creations cannot both insert, and compares the stored task
set and policy against the request rather than resuming blindly.

Claiming is one statement: the inner SELECT ... FOR UPDATE SKIP LOCKED
picks the oldest claimable task that matches the requested types while
stepping over rows another claimant holds, and the outer UPDATE mints the
lease and spends a start. Timestamps arrive as parameters from the
injected clock; this adapter never calls SQL now().

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 11: PostgreSQL adapter — `heartbeat`, `complete`, `fail`, `reap_expired`

**Files:**
- Modify: `oxo-tasks-postgres/src/lib.rs`

**Interfaces:**
- Produces: the four methods replacing their stubs. The three reporting calls must distinguish `UnknownTask`, `NotClaimed` and `LeaseLost` exactly as the in-memory adapter does — the conformance suite asserts it.

- [ ] **Step 1: Confirm the failing cases**

Run: `make verify-db`
Expected: the lease, failure and reap cases fail at `unimplemented!("Task 11")`. Capture the output.

- [ ] **Step 2: Implement a shared lease resolution**

Add to `impl PostgresTaskStore` in `oxo-tasks-postgres/src/lib.rs`:

```rust
    /// Resolve a lease against one task, distinguishing the three ways it can
    /// be invalid. A single query so the checks cannot drift apart between
    /// the three reporting calls, and so the answer cannot change between
    /// two of them.
    async fn resolve<'e, E>(executor: E, lease: Lease) -> Result<(), TaskStoreError>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row: Option<(String, Option<Uuid>)> =
            sqlx::query_as("SELECT state, lease_token FROM tasks WHERE id = $1")
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
```

- [ ] **Step 3: Implement the three reporting calls**

```rust
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

        let (attempts, max_attempts, backoff_secs): (i32, i32, i64) = sqlx::query_as(
            "SELECT j.attempts, r.max_attempts, r.backoff_secs \
             FROM tasks j JOIN jobs r ON r.id = j.job_id WHERE j.id = $1",
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
```

- [ ] **Step 4: Implement `reap_expired`**

```rust
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
            "UPDATE tasks AS j SET \
                 state = CASE WHEN j.attempts >= r.max_attempts THEN 'abandoned' ELSE 'pending' END, \
                 claimable_at = CASE WHEN j.attempts >= r.max_attempts THEN j.claimable_at \
                                     ELSE $1 + (r.backoff_secs * interval '1 second') END, \
                 lease_token = NULL, claimed_by = NULL, claimed_at = NULL, \
                 last_heartbeat_at = NULL \
             FROM jobs AS r \
             WHERE r.id = j.job_id AND j.state = 'claimed' \
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
git add oxo-tasks-postgres/
git commit -F - <<'EOF'
feat(tasks-pg): report on held tasks and reclaim dead ones

The three reporting calls resolve a lease through one shared query, so the
distinction between an unknown task, an unclaimed one and a lost lease
cannot drift apart between them — the conformance suite asserts all three
against both adapters.

Reaping is a single UPDATE ... FROM jobs so two concurrent reapers cannot
double-count: each expired row is claimed by exactly one of them. The CASE
spends the start already consumed at claim time, abandoning when the
budget is gone and requeueing with the job's snapshotted backoff
otherwise, so a tile that kills its worker cannot be re-claimed instantly.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 12: PostgreSQL adapter — the gate, throughput, and a green suite

**Files:**
- Modify: `oxo-tasks-postgres/src/lib.rs`

**Interfaces:**
- Produces: `job_status` and `throughput` replacing their stubs. After this task no `unimplemented!` remains in either crate and the whole conformance suite passes against both adapters.

- [ ] **Step 1: Confirm the failing cases**

Run: `make verify-db`
Expected: the three remaining cases fail at `unimplemented!("Task 12")`.

- [ ] **Step 2: Implement both, sharing one tally**

`tally` is an inherent method, so it belongs in the `impl PostgresTaskStore`
block alongside `new` and `resolve` — not in `impl TaskStore`, which may
contain only the trait's own methods. The two port methods below it go in
`impl TaskStore`.

```rust
    /// Count tasks per state for one job, in one query. Shared by the gate
    /// and the throughput snapshot so the two cannot disagree.
    async fn tally(&self, job_id: JobId, now: chrono::DateTime<chrono::Utc>) -> Result<Throughput, TaskStoreError> {
        let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM jobs WHERE id = $1")
            .bind(job_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(adapter)?;
        if exists.is_none() {
            return Err(TaskStoreError::UnknownJob { job_id });
        }

        let (pending, claimable_now, claimed, succeeded, abandoned): (i64, i64, i64, i64, i64) =
            sqlx::query_as(
                "SELECT \
                     count(*) FILTER (WHERE state = 'pending'), \
                     count(*) FILTER (WHERE state = 'pending' AND claimable_at <= $2), \
                     count(*) FILTER (WHERE state = 'claimed'), \
                     count(*) FILTER (WHERE state = 'succeeded'), \
                     count(*) FILTER (WHERE state = 'abandoned') \
                 FROM tasks WHERE job_id = $1",
            )
            .bind(job_id.as_uuid())
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
    async fn job_status(&self, job_id: JobId) -> Result<JobStatus, TaskStoreError> {
        let now = self.clock.now();
        let tally = self.tally(job_id, now).await?;

        if tally.pending == 0 && tally.claimed == 0 {
            return Ok(if tally.abandoned == 0 {
                JobStatus::Complete
            } else {
                JobStatus::Failed {
                    abandoned: tally.abandoned,
                }
            });
        }

        Ok(JobStatus::InProgress {
            pending: tally.pending,
            claimed: tally.claimed,
            succeeded: tally.succeeded,
            abandoned: tally.abandoned,
        })
    }

    async fn throughput(&self, job_id: JobId) -> Result<Throughput, TaskStoreError> {
        let now = self.clock.now();
        self.tally(job_id, now).await
    }
```

- [ ] **Step 3: Run the whole suite against both adapters**

Run: `make verify`
Expected: PASS, including the 15 in-memory conformance tests.

Run: `make verify-db`
Expected: PASS, all 15 conformance cases plus the migration test — 16 tests against a real PostgreSQL.

- [ ] **Step 4: Confirm no stubs survive**

```bash
grep -rn 'unimplemented!' oxo-tasks/src oxo-tasks-postgres/src
```
Expected: no output. If anything matches, a method was never implemented and the suite is not covering it — report that rather than removing the grep.

- [ ] **Step 5: Confirm the dependency boundary held**

```bash
cargo tree --package oxo-tasks --edges normal | grep -iE "sqlx|postgres|tokio-postgres|reqwest|hyper"
```
Expected: no output. The whole point of the crate split is that a consumer can depend on the port without the adapter's dependencies. If `sqlx` has reached `oxo-tasks`, the design is broken — stop and report it.

- [ ] **Step 6: Commit**

```bash
git add oxo-tasks-postgres/
git commit -F - <<'EOF'
feat(tasks-pg): answer the completion gate and the throughput snapshot

Both read one aggregate query with FILTER clauses, so the gate and the
snapshot cannot disagree about a job. An unknown run is refused rather
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
- `cargo tree --package oxo-tasks --edges normal` shows no database or network dependency.

Five design-document open decisions are deliberately untouched:

- **The per-tile resource footprint model** — awaits spike 0's measurements. The task-type filter is the interim capacity hook; the estimate later becomes a column and a claim predicate, which is additive.
- **Claim fairness across concurrent jobs** — first-in-first-out by `claimable_at` across all jobs means an older job starves a newer one. Acceptable while one region is produced at a time.
- **Whether a reap should ever be exempt from consuming an attempt** — a node reboot and an out-of-memory kill are indistinguishable today.
- **Whether a do-not-retry hint earns its place** — an added optional field if sub-project 4's entry point turns out to recognise terminal cases.
- **The exact metric set behind `throughput`** — a snapshot of counts here; rates are the consumer's to derive by differencing successive snapshots, and the shape should settle with sub-project 3 as the first real consumer.

Two things sub-project 3 inherits and must honour:

- **Construct a `RegionSpec` only via `validate`/`from_toml`.** Nothing in the type system enforces it.
- **`reap_expired` is not self-driving.** Something must call it periodically, and that something is the control plane. A store whose reaper is never invoked never reclaims anything.
