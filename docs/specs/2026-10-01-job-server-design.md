# Job server design

Sub-project 2 of the decomposition in
[the architecture design](2026-10-01-oxo-architecture-design.md): durable
task state, lease and claim semantics, retry accounting, and the
region-completion gate, behind an explicit port with a PostgreSQL adapter
behind it.

The architecture document governs. Where this document appears to
contradict it, this document is wrong — except for its open decision
assigning the per-tile resource footprint model to this sub-project,
which this design argues cannot live here and which is amended there
accordingly.

## Goal

A durable task store that hands each unit of tile production to exactly
one worker at a time, reclaims work from workers that die, retries under
a declared policy, and can state whether a region is finished.

Success:

- A worker that dies mid-task has its work reclaimed and reissued, and
  cannot later report on the task it lost.
- Resubmitting the same specification revision resumes a job rather than
  duplicating its work.
- Restarting the process loses no task state.
- A tile that repeatedly kills its worker is eventually abandoned rather
  than cycling forever.
- An operator can see that a region has become unachievable without
  waiting for the rest of it to finish.
- The whole port is exercisable without a database.

## Scope

In: the port, its domain types, an in-memory adapter, a PostgreSQL
adapter, and the conformance suite both must satisfy.

Out: the HTTP claim API and the planner that computes a job's task set
(both sub-project 3), the per-tile resource footprint model (below), any
actuation of the throughput signal, and cross-job reuse of completed
work.

## Shape: two crates, and why

| Crate | Contents | Depends on |
|---|---|---|
| `oxo-tasks` | The port trait, its request and response types, the task state machine, an in-memory adapter | `oxo-spec` |
| `oxo-tasks-postgres` | The PostgreSQL adapter | `oxo-tasks`, `sqlx` |

The split is the point. `oxo-tasks` has no database dependency at all, so
the control plane can depend on the abstraction without `sqlx` entering
its dependency graph; only the composition root names the adapter. One
crate behind a cargo feature would compile the same but would let the
concrete choice leak into every consumer, which is the coupling the port
exists to prevent.

`oxo-tasks` reuses `oxo-spec`'s `TileId` rather than defining its own.
`TileId` is a validated value type with no I/O and no behaviour beyond
identity and formatting, and inventing a parallel one would reintroduce
exactly the translation layer the tile-identifier decision rejected. The
dependency points from tasks to the specification model and never back.

## The port

An async trait whose every method takes an owned request and returns an
owned response, with no transaction spanning a call. That constraint is
deliberate: it keeps a network adapter a mechanical addition rather than
a redesign, and it rules out exposing `begin → select → update → commit`
across the API.

| Method | Purpose |
|---|---|
| `create_job` | Register a job and its task set. Idempotent on `(region_code, revision)`. |
| `claim` | Hand one claimable task to a worker, minting a lease token. |
| `heartbeat` | Assert a lease is still held, and learn if it was revoked. |
| `complete` | Mark a task succeeded. |
| `fail` | Record a failure; requeue with backoff or abandon. |
| `reap_expired` | Reclaim tasks whose heartbeat lapsed or which exceeded the maximum duration. |
| `job_status` | The completion gate. |
| `throughput` | Queue depth and claim, completion and failure rates. |

`create_job` takes the task set as input:
`{region_code, revision, max_attempts, backoff_seconds, tasks: Vec<(TileId, TaskType)>}`.
Computing that set — atomizing a specification into up to 2N tasks — is
the planner's work in sub-project 3. This subsystem stores what it is
given.

## Lease tokens

**Every claim mints a fresh lease token, and `heartbeat`, `complete` and
`fail` all require it.** A mismatch is rejected, distinctly, as its own
outcome rather than as a generic error.

This is the single most important invariant here. Without it, a worker
that stopped heartbeating, had its task reclaimed, and then came back to
life could mark that task complete — while another worker is hours into
redoing it. The region's gate would then open on a tile nobody finished.
A token turns that race into a clean, detectable rejection, and the
rejection is non-retryable by construction: the caller has lost the task
and must stop, not try again.

## States, and where attempts increment

```
                    claim
     ┌──────────┐ ─────────▶ ┌──────────┐ ── complete ──▶ ┌───────────┐
     │ Pending  │            │ Claimed  │                 │ Succeeded │
     └──────────┘ ◀───────── └──────────┘ ── fail,  ─────▶ └───────────┘
          ▲        fail or        │         attempts
          │        reap, with     │         exhausted      ┌───────────┐
          │        attempts       │                ───────▶│ Abandoned │
          └────────remaining ─────┘                        └───────────┘
```

`Pending` carries a `claimable_at` instant, so a failed task becomes
pending immediately but is not claimable until its backoff elapses.
**A reaped task takes the same backoff as a reported failure.** Without
it, a tile that kills its worker would be re-claimed the instant it is
reclaimed, burning its whole attempt budget in as long as it takes to
crash three times — which on an out-of-memory kill could be minutes.
`Succeeded` and `Abandoned` are terminal.

**Attempts increment at claim time, not at failure.** This is the
load-bearing detail. It makes a reaped task consume an attempt with no
extra bookkeeping, which matters because the likeliest reason a heartbeat
stops is the task killing its own worker — mesh generation exhausting
memory, or a tile filling the disk. Those are the failure modes this
project exists to handle. Were reaps free, a tile that reliably kills its
worker would cycle forever, consuming the fleet.

The cost is real and accepted: a transient node reboot charges the tile
an attempt it did not deserve. With count-only retries there is no way to
tell the two apart, and the asymmetry favours terminating rather than
looping.

`attempts` therefore counts **starts**, and `max_attempts` bounds starts.

## Heartbeat expiry, with a backstop

A claim is held for as long as heartbeats arrive. `reap_expired` reclaims
a task when none has arrived for a configured number of intervals.

That interval, the number tolerated, and the maximum-duration backstop
are **server configuration, not per-job and not per-specification**.
Putting them in the specification was considered and rejected below: they
are operational tuning, and an operator authoring a region is the person
least placed to choose them. Snapshotting them onto the job as the
failure policy is would also freeze an operational knob into a
weeks-long artifact.

No fixed lease duration, because nobody can state one correctly in
advance: tile build time varies by zoom level, provider and terrain, and
an ortho task runs for hours where an overlay task runs for minutes. A
single duration would either kill long ortho builds or leave dead overlay
tasks held for hours. Heartbeats are self-calibrating — a dead worker is
detected within a minute either way.

**A maximum duration remains as a backstop**, because a worker that is
wedged but alive — blocked on a socket read, say — keeps heartbeating and
would otherwise hold its task indefinitely. Exceeding it is reaped exactly
as a lapsed heartbeat is, and consumes an attempt the same way.

## The failure policy is snapshotted

`max_attempts` and `backoff_seconds` are copied onto the job at
`create_job` rather than read from the specification each time. Editing a
specification therefore cannot retroactively change the policy of a job
already in flight, which matters when a job lasts weeks.

## Schema

```
jobs
  id            uuid primary key
  region_code   text        ─┐ unique together
  revision      integer     ─┘
  max_attempts  integer        snapshot of the spec's failure policy
  backoff_secs  bigint         snapshot
  total_tasks    integer
  created_at    timestamptz

tasks
  id                uuid primary key
  job_id            uuid references jobs
  tile              text           canonical TileId form
  task_type          task_type       'ortho' | 'overlay'
  state             task_state      'pending'|'claimed'|'succeeded'|'abandoned'
  attempts          integer        starts, not failures
  claimable_at      timestamptz    gates backoff
  lease_token       uuid           null unless claimed
  claimed_by        text           worker identity, null unless claimed
  claimed_at        timestamptz    for the maximum-duration backstop
  last_heartbeat_at timestamptz
  last_failure      text           null until a failure is recorded
  unique (job_id, tile, task_type)
```

A partial index on `(claimable_at, id) where state = 'pending'` serves
the claim query; one on `(last_heartbeat_at) where state = 'claimed'`
serves the reaper. Both are the hot paths and nothing else is.

## Claiming is one statement

```sql
UPDATE tasks SET
    state = 'claimed', lease_token = $1, claimed_by = $2,
    claimed_at = $4, last_heartbeat_at = $4, attempts = attempts + 1
WHERE id = (
    SELECT id FROM tasks
    WHERE state = 'pending' AND claimable_at <= $4
      AND ($3::task_type[] IS NULL OR task_type = ANY($3))
    ORDER BY claimable_at, id
    FOR UPDATE SKIP LOCKED
    LIMIT 1
)
RETURNING id, job_id, tile, task_type, attempts;
```

`FOR UPDATE SKIP LOCKED` is PostgreSQL's own documented idiom for
handing one row to exactly one of many concurrent consumers. Ordering is
first-in-first-out by `claimable_at`.

The optional task-type filter is how a worker expresses capacity until
there is a footprint model: a worker short on disk claims overlay work
only, since an overlay task is a file copy and a conversion where an ortho
task is hundreds of gigabytes.

## One injected clock, never the database's

**Every timestamp this subsystem reads or writes comes from an injected
`Clock`, passed into queries as a parameter. The PostgreSQL adapter never
calls SQL `now()`.**

Two reasons, and the second is the one that matters.

Testability: heartbeat expiry, backoff and the maximum-duration backstop
are all time-dependent, and a suite that must sleep to test them is slow
and flaky. A test clock advanced by hand makes every expiry case
deterministic and instant — and makes those cases testable against *both*
adapters, which a database-side `now()` would not be.

Correctness: with SQL `now()` the application and the database keep
separate clocks. A worker's heartbeat would be judged against database
time while its own timeout logic used host time, so skew between the two
silently changes when a task is reclaimed. One clock removes the question.

## The completion gate

`job_status` returns one of three shapes:

- **`Complete`** — every task succeeded.
- **`Failed { abandoned }`** — nothing is left to run and at least one
  task was abandoned. The region can never complete.
- **`InProgress { pending, claimed, succeeded, abandoned }`** — work
  remains.

`abandoned` is deliberately visible *during* `InProgress`. A region with
an abandoned tile is already unachievable, and an operator should learn
that within minutes so they can correct the specification and start a new
job — not discover it after a fortnight of other tiles finishing. Work on
the remaining tiles continues regardless; whether to stop early is the
operator's call, not this subsystem's.

## Error handling

`TaskStoreError` distinguishes, at minimum: a lease-token mismatch (the
caller has lost the task and must not retry), an unknown task or job, a
state-machine violation (completing an unclaimed task), and an adapter
failure (which may be retryable). The first three are deterministic
outcomes of a correct store and must be representable without the caller
guessing from a string.

## Testing

**One conformance suite, run against both adapters.** That is what makes
the port real rather than nominal, and what stops the in-memory adapter
drifting into a fiction that passes while the real one would not. The
suite covers the invariants rather than the implementation: a task is
claimed by at most one worker; a stale lease token is rejected on
heartbeat, complete and fail; a reap consumes an attempt; backoff delays
claimability; abandonment happens at exactly `max_attempts` starts;
`create_job` is idempotent; the gate's three shapes.

`make verify` runs the in-memory suite and all unit tests — fast, no
services. `make verify-db` starts a disposable PostgreSQL via podman,
runs the identical suite against `oxo-tasks-postgres`, and tears it down.

**The PostgreSQL tests are excluded by name, not skipped at runtime.**
A runtime skip is how a suite reports green while testing nothing — the
exact trap sub-project 1 hit, where a renamed Gherkin step turned a
scenario into a silent skip at exit code 0. Exclusion by name means the
absence of database coverage is visible in which target you ran.

Concurrency deserves a real test rather than an argument: the suite
should claim from many tasks at once against the same job and assert that
every task was handed out exactly once.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Deployment shape | A library crate, service-ready by construction | Workers already talk to the control plane, so a second hop buys nothing now; owned request/response types with no transaction spanning a call keep a network adapter mechanical |
| Crate split | `oxo-tasks` (port, types, in-memory adapter) and `oxo-tasks-postgres` (adapter) | The consumer depends on the abstraction and only the composition root names the adapter; one crate behind a feature would leak `sqlx` into every consumer |
| Tile type | Reuse `oxo-spec`'s `TileId` | A validated value type with no I/O; a parallel type would reintroduce the translation layer the tile-identifier decision rejected |
| Queue layer | A domain `tasks` table claimed with `SELECT … FOR UPDATE SKIP LOCKED` | No mature Rust PostgreSQL queue exists (pgmq's client is alpha, apalis is a release candidate with a version-skewed backend, the rest are 0.x), and none can answer this system's domain questions; a generic queue plus a state table is more bespoke glue than one table, not less |
| Lease model | Heartbeat expiry with no fixed duration, plus a maximum-duration backstop | No correct duration can be stated in advance, and ortho and overlay tasks differ by orders of magnitude; the backstop covers a worker that is wedged but alive |
| Lease token | A fresh token per claim, required by heartbeat, complete and fail | Without it a revived worker can complete a task another worker now owns, opening a region's gate on a tile nobody finished |
| Attempt accounting | `attempts` increments at claim | Makes a reap consume an attempt with no extra bookkeeping; the likeliest cause of a lapsed heartbeat is the task killing its own worker, and free reaps would let such a tile cycle forever |
| Retry model | Count starts only, no failure classification | Ortho4XP's headless path reports a bare `Crash!` with no traceback, so a required classification would be filled with a guess |
| Failure policy | Snapshotted onto the job at creation | Editing a specification must not retroactively change a job in flight, which matters across a weeks-long build |
| Job identity | `(region_code, revision)`; the same revision resumes idempotently, a new revision is a fresh job | Makes resume-after-interruption work — the case that actually happens — while keeping cross-run reuse, which is incremental production, out of scope |
| Clock | One injected `Clock`; timestamps passed as query parameters, never SQL `now()` | Makes every expiry and backoff case deterministically testable against both adapters, and removes application-versus-database clock skew from when a task is reclaimed |
| Trait dispatch | `async_trait`, so the port is dyn-compatible | The composition root injects the adapter behind a trait object; native async-fn-in-trait is also unavailable under the project's declared 1.74 MSRV, which stabilised it in 1.75 |
| Claim ordering | First-in-first-out by `claimable_at` | Simplest fair order; the task-type filter is how a worker expresses capacity until a footprint model exists |
| Gate shape | `Complete` / `Failed` / `InProgress`, with abandoned counts visible throughout | An unachievable region should be visible in minutes, not after a fortnight |
| Conformance testing | One suite against both adapters; PostgreSQL excluded by name from `make verify` | A shared suite stops the fake drifting; exclusion by name rather than a runtime skip prevents a green run that tested nothing |

## Open decisions

- **The per-tile resource footprint model.** The architecture document
  assigns it here, but it cannot be built yet: it needs spike 0's
  measurements, and its consumer is the worker's capacity self-check
  rather than this store. The task-type filter is the interim hook. When
  the numbers exist, the estimate becomes a column and a claim predicate,
  which is additive.
- **Claim fairness across concurrent jobs.** First-in-first-out by
  `claimable_at` across all jobs means an older job starves a newer one.
  Acceptable while one region is produced at a time; needs a policy
  before two jobs run concurrently.
- **Whether a reap should ever be exempt from consuming an attempt.** A
  node reboot and a tile that exhausts memory are indistinguishable
  today. If worker telemetry later separates them, an exemption is a
  narrow change.
- **Whether a do-not-retry hint earns its place.** Rejected for v1
  because the only honest classification of `Crash!` is unknown. If the
  worker's custom entry point turns out to recognise genuinely terminal
  cases, an optional hint is an added field, not a redesign.
- **The exact metric set behind `throughput`.** Sub-project 3 consumes it
  and sub-project 5 renders it; the shape should be settled with the
  first real consumer rather than guessed here.

## Out of scope

- **The HTTP claim API and the planner.** Sub-project 3 owns both. This
  subsystem never speaks HTTP and never computes a task set.
- **Resource footprint estimation.** Above.
- **Throughput actuation.** This subsystem reports; nothing scales.
- **Incremental production.** No reuse of completed work across jobs,
  per the architecture document.
- **Alerting.** The specification carries alert destinations; sub-project
  5 acts on them.

## Rejected alternatives

**A separate service with a wire protocol.** The boundary would be a
network contract rather than a Rust trait, allowing a non-Rust
replacement and independent scaling. Rejected: workers already talk to
the control plane, so this adds a hop nobody needs, plus a second
protocol to version and test and a second deployable — for optionality
that the owned-types constraint preserves anyway.

**A generic queue crate alongside a domain table.** Satisfies the
"no bespoke job or task management system" non-goal by the letter. Rejected: two
sources of truth needing consistency, an alpha or release-candidate
dependency in the critical path, visibility-timeout semantics that fight
heartbeat expiry, and more glue in total than the single-table design
contains.

**Adopting a queue framework and bending the model to it.** Least code
owned. Rejected: fixed visibility timeouts instead of heartbeat expiry,
a completion gate that becomes awkward or impossible to express, and a
pre-1.0 dependency dictating the shape of the system's core.

**A fixed lease duration per task type.** An explicit ceiling, and a
wedged worker cannot exceed it. Rejected: the numbers are guesses until
spike 0 runs, a tile that legitimately runs longer than its lease is
killed and retried forever, and every new task type needs another number.

**Lease duration declared in the region specification.** Part of the
reproducible package definition. Rejected: it pushes an operational
tuning decision onto whoever authors the specification, who is least
placed to know, and bakes a wrong value into the artifact.

**Requiring a failure classification on every failure.** Forces the
question to be answered deliberately. Rejected: the only honest
classification of `Crash!` is unknown, so a required field would be
filled with a guess, which is worse than not asking.

**Revision as metadata, with resubmission reconciling against existing
tasks.** Never re-runs work unnecessarily. Rejected: it is incremental
production, which is out of scope, and it needs a parameter-fingerprint
model to decide what a change invalidates.

**No `jobs` table, keying tasks directly.** Less machinery. Rejected:
job-level state has nowhere to live, so the gate and the throughput
signal become repeated aggregates with no record of when a job began or
what it concluded.

**`testcontainers` inside the ordinary suite.** One target, nothing to
remember. Rejected: every `make verify` would need a container runtime
and an image pull, turning a sub-second suite into a slow one, and
podman's Docker-compatible socket is a setup step that fails
confusingly when absent.

**Requiring `DATABASE_URL` for `make verify`.** Impossible to forget.
Rejected: it breaks the fast edit-test loop the Makefile exists to
provide, and makes a documentation change require infrastructure.

## Dependencies

- **`oxo-spec`** — `TileId` and the failure-policy values. No runtime
  coupling beyond those types.
- **`sqlx`** — in `oxo-tasks-postgres` only.
- **PostgreSQL** — the v1 adapter's store. A deployment dependency of
  that adapter, not of the design.
- **podman** — tests only, for a disposable database.

## Related

- [OXO architecture design](2026-10-01-oxo-architecture-design.md) — the
  execution model and the decomposition. Its Task substrate decision is
  amended by this design.
- [Region specification design](2026-10-01-region-spec-design.md) —
  sub-project 1, which supplies `TileId` and the failure policy.
