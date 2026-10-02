# Control plane design

Sub-project 3 of the Ortho4 XEarthLayer Orchestrator: the planner that
atomizes a region specification into tasks, and the HTTP API that workers
claim and report through.

Argued from `2026-10-01-oxo-architecture-design.md`, which carries the
execution model this document assumes: pull dispatch, two task types, the
job-server port of `2026-10-01-job-server-design.md` underneath. Where this
document contradicts the architecture document, the architecture document
is wrong and must be amended, not silently diverged from.

## Goal

An operator hands OXO a validated region specification and gets back a job
they can watch; a worker pod asks OXO for work and gets one task, a lease,
and the obligation to heartbeat. Everything between those two sentences —
atomizing the specification, registering the job, serving the claim and
report calls, reclaiming work from dead workers, answering the completion
gate and the throughput signal over the wire — is this sub-project.

## Scope

In: the planner (specification → task set), job submission, the worker
protocol (claim, heartbeat, complete, fail) over HTTP, job status and
throughput over HTTP, job identity recovery (`find_job`), the reaper that
drives lease expiry, the server binary and its configuration, and the port
amendments the job server's final review settled for this sub-project.

Out, **amended from the architecture document's row for sub-project 3**:
the pod spec and configuration injection. Both were assigned here, but
spike 0 (the Ortho4XP pod contract) has not run and sub-project 4's worker
image — the thing a pod spec would describe and configuration would be
injected into — does not exist. Designing either now would be invention. They
move to sub-project 4, where the image, the injection surface and the pod
spec can be designed against each other. Ruled with the operator
2026-10-02; the architecture document's decomposition table is amended in
the same change that lands this document.

## Shape: two crates, and why

| Crate | Role |
|---|---|
| `oxo-control` | Library: the planner, the HTTP API as an `axum::Router` over an injected `Arc<dyn TaskStore>`, the wire types, the error mapping, and the reaper loop. Depends on `oxo-spec`, `oxo-tasks`, `axum`, `tokio`, `serde` — **never on `sqlx` or `oxo-tasks-postgres`**. |
| `oxo-controld` | Binary: the composition root. Parses configuration, connects the PostgreSQL adapter, builds the router, spawns the reaper, serves. The only new code that knows PostgreSQL exists. |

The split repeats the pattern the workspace already has twice
(`oxo-spec`/`oxo-spec-cli`, `oxo-tasks`/`oxo-tasks-postgres`), and for the
same reason: everything with behaviour worth testing lives in a library
that `make verify` exercises in full against the in-memory adapter and a
`TestClock`, while the binary is wiring thin enough to read. `oxo-controld`
compiles under `make verify` (building the PostgreSQL adapter needs no
database; only its tests do) but carries no test that touches a database.

## The planner

One pure function in `oxo-control::planner`:

```rust
pub fn plan(spec: &RegionSpec) -> Result<CreateJob, PlanError>
```

- One ortho task per tile; one overlay task per tile additionally when
  `parameters.include_overlays` is true. Up to 2N tasks from N tiles,
  exactly as the architecture document requires. `include_overlays = false`
  yields N ortho tasks and is a first-class choice.
- Identity and policy are copied from the specification:
  `metadata.region_code`, `metadata.revision`,
  `failure_policy.max_attempts`, `failure_policy.backoff_seconds`.
- Task order is deterministic — tiles in `BTreeSet` order, each tile's
  ortho task before its overlay task — so re-planning an unchanged
  specification reproduces the task set element for element and
  `create_job`'s idempotency resumes the job instead of conflicting.
  (The store compares task sets order-insensitively; determinism here is
  about reproducibility, not a store requirement.)
- `PlanError` has exactly the cases the checked port constructors can
  refuse (`max_attempts` of zero, a backoff beyond what every adapter can
  represent). Both are unreachable from a **validated** `RegionSpec` —
  `oxo-spec` already refuses `max_attempts = 0` — but the planner's input
  type cannot prove that, so the error is propagated, never unwrapped.

The planner never revalidates tiles: `RegionSpec` holding `TileId` means
every tile is in range by construction, which is the promise `oxo-spec`
made for exactly this consumer.

## Port amendments executed here

The job server's final review settled six questions for this sub-project
(`2026-10-01-job-server-design.md`, "Settled for sub-project 3, not
open"). They are implemented first, because the control plane is built on
the amended surface. The shape:

- **`BackoffSeconds` and `TimeoutSeconds`** replace `std::time::Duration`
  on the request surface. Both are whole-second newtypes over `u64` with
  checked constructors that refuse anything no adapter can represent
  faithfully (the bound is chrono-arithmetic representability).
  `TimeoutSeconds` additionally refuses zero: a zero heartbeat timeout or
  max duration means "reclaim every in-flight task on the next tick",
  which is never what an operator meant. `BackoffSeconds` **permits
  zero**: `oxo-spec` defaults `backoff_seconds` to 0 and "retry
  immediately" is legal intent. This is a deliberate narrowing of the
  settled answer's "refuses zero", which did not account for the
  specification's own default; the two reap durations were its real
  target. `CreateJob.backoff` becomes `BackoffSeconds`;
  `ReapRequest.heartbeat_timeout` and `.max_task_duration` become
  `TimeoutSeconds`. All five `from_std(…).unwrap_or(zero())` conversion
  sites disappear, and with them the inversion where an enormous value
  silently became zero.
- **`MaxAttempts`**, a newtype over `NonZeroU32` with a checked
  constructor, replaces `CreateJob.max_attempts: u32`. Zero was an
  inconsistent state — claimable but never retryable — and now cannot be
  constructed.
- **The reclaim contract is documented honestly.** `heartbeat`, `complete`
  and `fail` documentation names both `NotClaimed` (reaped, not yet
  re-claimed) and `LeaseLost` (re-claimed by someone else) as the two
  spellings of "you have lost this task; stop working".
- **`find_job` joins the port**: a `FindJob { region_code, revision }`
  request returning `Option<JobId>`, so a restarted control plane recovers
  a job's handle without re-running the planner against `created: false`.
  Held to conformance like every other method, in both adapters.
- **The conformance gaps get cases**: double-`complete` returns
  `NotClaimed`; the policy arm of `JobConflict`; two revisions of one
  region are separate jobs; task-set comparison is order-insensitive;
  claims are first-in-first-out among claimable tasks.

## The HTTP surface

`axum` 0.8, JSON bodies except where noted, rooted at `/api/v1`. The wire
types are owned by `oxo-control` and serialized with `serde`; the port
types in `oxo-tasks` stay serde-free, so the wire contract can change
without touching the port. Mapping between the two is explicit and lives
beside the handlers.

### Operator endpoints

| Method and path | Request | Responses |
|---|---|---|
| `POST /api/v1/jobs` | Body: region specification **TOML**, exactly the format `oxo-spec validate` accepts | `201` `{job_id, created: true, total_tasks}`; `200` with `created: false` when the job already existed and was resumed; `422` with the full validation report when the specification fails to parse or validate; `409` on `JobConflict` |
| `GET /api/v1/jobs?region_code=NA&revision=1` | — | `200` `{job_id}`; `404` when no such job |
| `GET /api/v1/jobs/{job_id}` | — | `200` job status (below); `404` |
| `GET /api/v1/jobs/{job_id}/throughput` | — | `200` `{pending, claimable_now, claimed, succeeded, abandoned}`; `404` |
| `GET /healthz` | — | `200` |

Submission takes TOML, not JSON, because the specification already has
exactly one canonical format with one validator, and a second accepted
encoding is a second thing to keep from drifting. The response to a faulty
specification is the same all-faults report the CLI prints, so an operator
fixes everything in one round trip.

Job status is a tagged object mirroring `JobStatus`:
`{"state": "complete"}`, `{"state": "failed", "abandoned": n}`, or
`{"state": "in_progress", "pending": n, "claimed": n, "succeeded": n,
"abandoned": n}`.

### Worker endpoints

| Method and path | Request | Responses |
|---|---|---|
| `POST /api/v1/claims` | `{worker, task_types?}` — `task_types` absent means any; present-but-empty means none, matching the port | `200` `{task_id, job_id, lease_token, tile, task_type, attempt}`; `204` when nothing is claimable (not an error; the worker sleeps and asks again); `422` `unknown_task_type` for an unrecognized entry in `task_types` |
| `POST /api/v1/tasks/{task_id}/heartbeat` | `{lease_token}` | `204`; `409`; `404` |
| `POST /api/v1/tasks/{task_id}/complete` | `{lease_token}` | `204`; `409`; `404` |
| `POST /api/v1/tasks/{task_id}/fail` | `{lease_token, reason}` | `200` `{outcome: "requeued", claimable_at, attempts_remaining}` or `{outcome: "abandoned"}`; `409`; `404` |

`tile` on the wire is the tile's canonical string form (`+50-002`), and
`task_type` is the canonical lowercase name — both already defined by
`oxo-spec` and `oxo-tasks`, not invented here.

### Error mapping

One table, applied by a single `impl IntoResponse` so a handler cannot
choose its own mapping. Error bodies are `{"error": code, "message": text}`
with `text` being the `TaskStoreError` rendering.

| `TaskStoreError` | Status | `error` code |
|---|---|---|
| `LeaseLost` | `409` | `lease_lost` |
| `NotClaimed` | `409` | `not_claimed` |
| `UnknownJob`, `UnknownTask` | `404` | `unknown_job`, `unknown_task` |
| `JobConflict` | `409` | `job_conflict` |
| `DuplicateTask`, `EmptyJob` | `422` | `duplicate_task`, `empty_job` |
| `Adapter` | `503` | `adapter` |

The worker protocol rule, stated once and testable: **any `409` on a task
report means the worker has lost that task and must stop working on it.**
`lease_lost` and `not_claimed` are distinguished in the body for
operators reading logs, not for workers to branch on. `503` is the only
status a worker retries.

## The reaper

Lease expiry is enforced by `reap_expired`, and something must call it:
this is that something. `oxo-control::reaper` exposes an async loop —
injected store, injected `ReapRequest`, injected interval — and
`oxo-controld` spawns it as a tokio task beside the server. Each pass logs
the outcome when it reclaimed anything. The loop is library code so its
behaviour (calls the store every interval, keeps going when a pass returns
`Adapter` errors — a transient database outage must not kill lease
enforcement) is tested under `make verify` with the in-memory store;
the binary only spawns it.

Both reap bounds and the interval are server configuration, reaffirming
the job-server design's ruling: they are operational tuning, and the
region's author is the person least placed to choose them.

## Configuration

`oxo-controld` takes flags with environment-variable fallbacks (clap,
derive style, as `oxo-spec-cli` established):

| Flag | Env | Default |
|---|---|---|
| `--bind` | `OXO_BIND` | `127.0.0.1:8080` |
| `--database-url` | `DATABASE_URL` | required, no default |
| `--heartbeat-timeout-secs` | `OXO_HEARTBEAT_TIMEOUT_SECS` | `120` |
| `--max-task-duration-secs` | `OXO_MAX_TASK_DURATION_SECS` | `21600` (6 h) |
| `--reap-interval-secs` | `OXO_REAP_INTERVAL_SECS` | `30` |

The two reap defaults are stated guesses: spike 0 has not produced real
tile timings. They are configuration precisely so the guess is cheap to
correct. `--bind` defaults to loopback so that exposing the API beyond the
host is an explicit act.

Timeout flags are validated through `TimeoutSeconds` at startup, so a zero
or absurd value is refused before the server binds rather than discovered
as a reclaim storm.

## Authentication: none in v1, and said out loud

Workers and operators are on a trusted network in the deployment this
serves; the API carries no credentials and the server does no
authentication. The `worker` field on a claim is identification for the
operator's benefit, not authentication. When that stops being true, the
boundary is ready for a bearer token as middleware without touching
handlers. Recorded as an open decision rather than silently assumed.

## Observability

`tracing` in the library, `tracing-subscriber` initialised by the binary,
env-filter controlled (`RUST_LOG`). Spans per request via
`tower-http`'s trace layer; the reaper logs each pass that reclaims
anything. Metrics export beyond the throughput endpoint is sub-project 5's.

## Testing

- **Planner**: pure unit tests — N tiles in, N or 2N tasks out, order
  deterministic, policy copied faithfully, and property-style checks over
  edge tiles (the antimeridian and pole bounds `TileId` already enforces).
- **Handlers**: `tower::ServiceExt::oneshot` against the real `Router`
  wired to the in-memory store and a `TestClock` — no network, no
  sleeps. Every row of the error-mapping table is asserted, as is the
  `204`-means-no-work claim contract.
- **Reaper loop**: in-memory store, `tokio::time::pause`-driven intervals,
  asserting it reaps on schedule and survives adapter errors.
- **Port amendments**: the conformance suite grows the five new cases plus
  `find_job` coverage, and every case runs against both adapters —
  `make verify` for in-memory, `make verify-db` for PostgreSQL.
- **Acceptance**: a Gherkin feature (`oxo-control/features/`) run by
  cucumber, driving the full HTTP surface in-process: an operator submits
  a specification, workers claim and complete every task, the gate
  reports done; a failing tile burns its budget and the job reports
  failed; a resubmitted specification resumes rather than duplicates.
- **`oxo-controld`**: configuration parsing tests only. Wiring is
  exercised by bringing the binary up against `make pg-up` PostgreSQL in
  `make verify-db`'s orbit only if cheap; otherwise the binary stays
  smoke-tested by compilation and the adapter stays covered by its own
  conformance run.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| HTTP framework | `axum` 0.8 | tokio-native (the workspace already runs tokio), tower middleware, the current maintained line. Requires raising the workspace `rust-version` floor from 1.74 to 1.75 — a one-line change; the floor was inherited from sqlx 0.8's needs, not chosen for an external constraint, and the installed toolchain is 1.92. |
| Wire types | Owned by `oxo-control`, serde there only | Keeps `oxo-tasks` dependency-light and lets the wire contract evolve without amending the port. The port stays the boundary; the wire is a view of it. |
| Submission format | TOML, the canonical spec format, only | One format, one validator, no drift between encodings. |
| Zero seconds | Legal for backoff, refused for reap bounds | `oxo-spec` defaults `backoff_seconds` to 0 and immediate retry is meaningful; a zero reap bound is always a reclaim storm. Narrows the settled answer, which had not accounted for the spec default. |
| Crate split | `oxo-control` lib + `oxo-controld` bin | Same pattern as the two existing pairs; keeps every testable behaviour inside `make verify`, keeps PostgreSQL knowledge in the composition root. |
| Reaper placement | Loop in the library, spawned by the binary | The loop has testable behaviour (cadence, error survival); spawning is wiring. |
| Claim miss | `204`, not an error payload | "Nothing to do" is the normal idle state of a pull system, not a fault. |
| Auth | None in v1 | Trusted network; boundary left ready for bearer-token middleware. |
| Pod spec, config injection | Moved to sub-project 4 | No spike-0 numbers, no worker image to describe; designing them now is invention. Architecture doc amended. |

## Open decisions

- **Worker authentication.** A bearer token shared via the platform's
  secret mechanism is the likely v2 shape; nothing in v1 precludes it.
- **Configuration injection surface.** What a worker receives at claim
  time beyond the task itself (provider, zoom, raw Ortho4XP keys) is
  sub-project 4's to design against the worker image. The claim response
  is versioned under `/api/v1`, so adding fields is additive.
- **Throughput metric set.** Unchanged from the job-server design: counts
  now, settled with the first real consumer.
- **The reap defaults.** `120`/`21600` seconds are guesses until spike 0
  produces real tile timings.

## Out of scope

- **The pod spec and configuration injection.** Sub-project 4, per the
  scope amendment above.
- **Scaling actuation.** The throughput endpoint reports; nothing scales.
- **TLS.** Terminate it in front of the server if the network demands it.
- **The operator web interface.** Sub-project 5 consumes these endpoints.
- **Incremental production.** Unchanged hard boundary.

## Rejected alternatives

**Serde derives on the port types.** Less mapping code. Rejected: it welds
the wire format to the port, so a wire change becomes a port change for
every adapter, and it hands `oxo-tasks` a dependency it does not need.
The mapping layer is small and lives in one module.

**Accepting JSON specifications as well as TOML.** Friendlier to
`curl | jq` habits. Rejected: two accepted encodings of the same document
means two parse paths to keep honest against one validator, for an
operator who already holds the TOML file `oxo-spec validate` checked.

**gRPC for the worker protocol.** Typed stubs, streaming. Rejected: the
worker protocol is four small unary calls; sub-project 5's HTML5 interface
wants plain HTTP anyway; and a second protocol toolchain is cost without a
consumer.

**Claim long-polling.** Fewer empty polls. Rejected for v1: it holds
server connections open to save workers a sleep loop they need anyway for
capacity checks; `204` plus client-side backoff is simpler and the wire
contract (idempotent claim) leaves room to add it later.

**The reaper as an external cron against a maintenance endpoint.** Less in
the server. Rejected: lease expiry is a correctness obligation of the
control plane, not optional maintenance; an operator forgetting the cron
silently disables retry-on-death.

**axum 0.7 to preserve the 1.74 floor.** No floor change. Rejected: it
starts new code on a superseded line whose upgrade (0.7 → 0.8 changed
path-parameter syntax) would land as churn in a later sub-project; the
floor exists to state what we need, not to be minimised for its own sake.

## Dependencies

New workspace dependencies: `axum` 0.8, `tower` (for `ServiceExt` in
tests), `tower-http` (trace layer), `serde_json`, `tracing`,
`tracing-subscriber`. All stable, post-1.0 or de-facto standard; no
alpha or release-candidate crates in the critical path, per the
architecture's non-goals.

## Related

- `docs/specs/2026-10-01-oxo-architecture-design.md` — the execution model
  and the decomposition this amends.
- `docs/specs/2026-10-01-job-server-design.md` — the port this builds on
  and the settled questions implemented here.
