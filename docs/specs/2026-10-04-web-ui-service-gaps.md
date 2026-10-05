# Web UI service gaps — review of the design handoff

Response to the mid-fi Figma handoff for OXO's web UI
(<https://www.figma.com/design/dJyiarI0mfqgfCVifUNlO4>, page "02 · Screens").
The handoff lists 20 capabilities the screens assume. Each is checked here
against the code at `d67a287`, corrected where the handoff got it wrong,
and given a proposed API or state change, with behaviour expressed as
Gherkin. This is design input to sub-project 5 (observability and operator
interface), not its design document. No UI is built from it yet.

**Status: decisions recorded 2026-10-04.** Sam ruled on the seven open
questions (see "Decisions" at the end). The gap sections below are
updated to match. Where a ruling replaced the original proposal, the
section says so.

## Summary

The handoff's reading of the code is mostly right. Six corrections matter:

1. **Jobs do not store their specification.** The `jobs` table holds
   `region_code`, `revision`, the snapshotted failure policy, `created_at`
   and the opaque worker payload — no name, no target root as a column, no
   TOML. The dashboard's name column, the job header and "Download TOML"
   for an existing job all need the spec persisted (gap 9).
2. **The job identifier is two fields, not one.** Identity is
   `(region_code, revision)`, unique together. The create form needs a
   revision field, and Cancel's type-to-confirm should type both.
3. **`GET /api/v1/jobs` exists, but as a lookup**, not a list: it takes
   `?region_code=&revision=` and returns a job id.
4. **The worker-name collision is not the `"oxo-worker"` fallback.** In a
   pod, the hostname defaults to the pod's name, so every pod started from
   the same YAML reports the same hostname, and the literal fallback is
   never reached. (Verified 2026-10-04: a `podman kube play` pod named
   `oxo-worker` reports hostname `oxo-worker`.) A suffix on "the shared default" cannot be detected; the fix is
   a per-boot unique id (gap 3).
5. **The failure reason is persisted** (`tasks.last_failure`, written by
   `fail`), but the reaper never writes one, so a reclaimed task shows the
   previous attempt's error or nothing (gap 12).
6. **Advanced settings are not validated until a worker runs them.** The
   spec checks `raw` keys and values for syntax only; type, enum and
   existence checks happen in the worker's runner at the `configure` phase,
   after a claim. The create form needs that catalogue up front (gap 16).

Two rulings amend recorded decisions in the job-server design: retry
adds a new task beside an abandoned one, which changes the
`UNIQUE (job_id, tile, task_type)` constraint and the completion gate
(gap 8), and cancel introduces a terminal job state (gap 7). A third
ruling introduces a new concept: bearer-token authentication on every
operation that changes production state (gap 19).

## Facts re-checked

| Handoff claim | Verdict | Evidence |
|---|---|---|
| Identity is `OXO_WORKER_NAME`, else hostname, else `"oxo-worker"` | Confirmed | `oxo-worker/src/config.rs:121-131` |
| `deploy/worker-pod.yaml` leaves `OXO_WORKER_NAME` unset | Confirmed | Listed under "deliberately left to their defaults" |
| Worker names are not UUIDs | Confirmed | Free text, `claimed_by text` |
| No worker table; `claimed_by` lost after a task ends | Confirmed | `complete`, `fail` and the reaper all null it (`oxo-tasks-postgres/src/lib.rs:384,412,427,473`) |
| A 204 claim stores nothing | Confirmed | `oxo-control/src/api/tasks.rs:12-37` |
| Reaper defaults 120 s / 6 h / 30 s; reclaims logged, not stored | Confirmed | `oxo-controld --help`; `reaper.rs` logs counts only |
| Task, job and lease ids are UUIDs | Confirmed | `oxo-tasks/src/ids.rs`, schema |
| No authentication in v1 | Confirmed | Control-plane design, "Authentication: none in v1"; the default bind is loopback |

Further facts the handoff didn't list and the proposals depend on:

- Task states are `pending`, `claimed`, `succeeded` and `abandoned`. There
  is no "running": `claimed` is running. `attempts` counts *starts*
  (incremented at claim), so a reclaimed task has spent one.
- Job state is **derived**, not stored: `complete` (nothing pending or
  claimed, none abandoned), `failed` (nothing pending or claimed, some
  abandoned), else `in_progress`. A job is not "failed" while anything is
  still running.
- Tasks have no `updated_at`, and jobs have no `started_at`/`updated_at`.
- Worker defaults: poll every 15 s when idle, heartbeat every 30 s.
- If a lease is lost mid-build, the worker kills the runner and delivers
  nothing. Egress happens *before* `complete`, so a lease lost during
  egress can leave a delivered tile whose completion is refused.
- Resubmitting an identical spec resumes the job (`created: false`); the
  same identity with a different task set or policy is `409 JobConflict`.

## Architectural note: a read port beside `TaskStore`

Most gaps are reads that the UI needs and the workers never do. Adding
them to `TaskStore` would force every adapter (and the 29-case
conformance suite) to carry UI concerns. The proposal is a second port in
`oxo-tasks`, `OperatorQueries` (interface segregation), implemented by the
same adapters with its own conformance cases. State changes the UI drives
(pause, cancel, retry) are writes with lifecycle rules, so they belong on
`TaskStore` and its conformance suite.

## Worker visibility (needs new state)

### 1. Worker last-seen — confirmed gap

Proposed table `workers (identity PRIMARY KEY, first_seen_at,
last_seen_at, last_task_types)`, keyed by the per-boot identity (gap 3) and upserted on every claim
(hits and 204 misses) and every heartbeat. A `task_types` value of `null`
means "any"; `["overlay"]` is an overlay-only worker.

```gherkin
Feature: Worker last-seen
  Scenario: An idle worker is visible
    Given no task is claimable
    When worker "fw1-a1" asks for work and receives 204
    Then worker "fw1-a1" is recorded as last seen now
    And its last task-type filter is "any"

  Scenario: An overlay-only worker is shown as such
    When worker "fw2-b2" claims with task_types ["overlay"]
    Then worker "fw2-b2" reports task types ["overlay"]

  Scenario: A heartbeat refreshes last-seen
    Given worker "fw1-a1" holds a lease
    When it heartbeats
    Then worker "fw1-a1" is recorded as last seen now
```

### 2. Attempt history — confirmed gap

Proposed table `attempts (id, task_id, job_id, worker,
attempt_no, claimed_at, ended_at, outcome, reason)`. One row is written at
claim; it is closed by `complete`, `fail`, the reaper, or a lease loss. The
handoff's outcome set is right, with one rename and one addition:

| Outcome | Written by |
|---|---|
| `succeeded` | `complete` |
| `failed` | `fail`, with the worker's reason |
| `reclaimed_heartbeat` | the reaper |
| `reclaimed_max_duration` | the reaper |
| `revoked` | cancel (gap 7) — new |
| `superseded` | a lease the store found already re-claimed — the handoff's `lease_lost`, renamed because the store only learns of it when a late call arrives, and may never |

This one table backs per-worker history, "who worked this finished task",
earlier-attempt errors, job `started_at` (first claim), and an ETA (median
succeeded duration per task type).

**Retention.** Rows are small: a 2,000-tile region with overlays and an
average of 1.5 attempts is about 6,000 rows. The proposal is to keep them
for the life of the job (they cascade with it) and to filter for the
UI's 24 h window at query time. There is no job deletion today, so a
retention policy only becomes necessary when deletion does.

```gherkin
Feature: Attempt history
  Scenario: A reaped attempt keeps its worker and reason
    Given worker "fw1-a1" claimed task T at 10:00
    And it has not heartbeated for longer than the heartbeat timeout
    When the reaper runs
    Then task T's attempt 1 has outcome "reclaimed_heartbeat"
    And it names worker "fw1-a1"
    And task T's last error reads "reclaimed: no heartbeat for 120 s"

  Scenario: A finished task still shows who did it
    Given worker "fw1-a1" completed task T
    Then task T's latest attempt names worker "fw1-a1" with outcome "succeeded"
```

### 3. Name collisions — confirmed; ruled: a per-boot unique id

The collision comes from the hostname, not from the literal fallback. A
pod's hostname defaults to its pod name (`oxo-worker` in
`deploy/worker-pod.yaml`), verified 2026-10-04 with `podman kube play`. The pod
name is identical on every host running that YAML, and the worker cannot
tell a shared hostname from a unique one, so the handoff's
suffix-on-the-shared-default rule cannot work.

**Ruling:** every worker process generates a unique identifier once per
boot. Depending on the runtime it is used either as a **suffix** on the
configured or host name, or **as the identity itself**:

- Under Podman, where one YAML gives every pod the same name, the
  identity is `<name>-<boot id>`, so the name stays readable and is
  still unique.
- Where the runtime supplies no meaningful name, the boot id is the
  identity.

Either way the identity is unique per process start. A restarted worker is
a new worker, so its history never merges with its predecessor's. Which
form a runtime uses is a worker-pod design detail. The wire type doesn't
change: `worker` stays one string.

```gherkin
Feature: Worker identity
  Scenario: Two pods from one spec are told apart
    Given two workers started from the same pod spec with no OXO_WORKER_NAME
    When both claim
    Then their reported identities differ

  Scenario: A restarted worker is a new worker
    Given a worker reported identity "oxo-worker-7f3a" and stopped
    When a worker starts again from the same pod spec
    Then its reported identity is not "oxo-worker-7f3a"
    And "oxo-worker-7f3a" eventually shows as gone quiet

  Scenario: The configured name stays readable
    Given a worker started with OXO_WORKER_NAME "fw1"
    Then its reported identity begins with "fw1"
```

### 4. Liveness thresholds — confirmed, with one correction

"Reaping" is a property of a **lease**, not a worker: a worker whose
lease is past the heartbeat timeout but not yet swept. The worker states
should be derived from last-seen and whether it holds a lease, and lease
health shown on the lease:

| Worker state | Rule (defaults) |
|---|---|
| Working | holds a lease, last heartbeat < 2 × heartbeat interval (60 s) |
| Late | holds a lease, last heartbeat between 60 s and the heartbeat timeout (120 s) |
| Reaping | holds a lease past the heartbeat timeout, awaiting the next sweep (≤ 30 s) |
| Idle | no lease, seen within 2 min (8 × the 15 s poll) |
| Gone quiet | not seen for 10 min |

Make the late, idle and gone-quiet windows `oxo-controld` flags beside the
existing reaper flags, and return the thresholds in the `GET /workers`
response so the UI never hard-codes them. Workers seen between 2 and
10 min with no lease are "Idle (stale)", or collapsed into Gone quiet;
that is a design call for the UI.

### 5. Read endpoints — confirmed gap

```
GET /api/v1/workers
  → { thresholds: {...}, workers: [ { identity, state, task_types,
       last_seen_at, lease: { task_id, job_id, tile, task_type,
       claimed_at, last_heartbeat_at } | null,
       last_24h: { succeeded, failed, reclaimed } } ] }
GET /api/v1/workers/{identity}/attempts?since=&cursor=
```

## Job control (buttons without endpoints)

All three need a stored job state. Proposed column `jobs.control text NOT
NULL DEFAULT 'active' CHECK (control IN ('active', 'paused',
'cancelled'))`. The derived status (`complete`/`failed`/`in_progress`)
stays derived, and the UI shows `cancelled` and `paused` over it.

### 6. Pause/resume — semantics confirmed

The handoff's semantics are right: a paused job's tasks are not
claimable, leases already held run to completion, and backoff clocks keep
running.

```gherkin
Feature: Pause and resume
  Scenario: A paused job hands out no work
    Given job J has pending tasks
    When the operator pauses job J
    Then no worker can claim a task of job J
    And tasks of other jobs remain claimable

  Scenario: Work in flight finishes
    Given worker W holds a lease on a task of job J
    When the operator pauses job J
    Then W's heartbeats are accepted
    And W can complete the task

  Scenario: Resume
    Given job J is paused
    When the operator resumes job J
    Then its pending tasks are claimable again
```

`POST /api/v1/jobs/{id}/pause` and `/resume`, both idempotent. Pausing a
cancelled or finished job is `409`.

### 7. Cancel — ruled: revoke leases, and cancelled is final

**Ruling:** cancelling a job invalidates every lease it holds, and
cancellation is final.

Revocation makes the worker's next heartbeat answer `409 LeaseLost`. The
worker kills its runner and delivers nothing, and the attempt closes as
`revoked`. A tile already in egress may still land, because egress
precedes `complete`; its completion is then refused. Finished tiles stay in
`target.root`, because OXO never deletes delivered artifacts.

Resubmitting the same spec returns the existing job with
`created: false` and state `cancelled`. To produce the region again, bump
the revision — the path the PNW E2E run already used. Retry is refused on
a cancelled job (gap 8).

```gherkin
Feature: Cancel
  Scenario: Cancel invalidates leases
    Given worker W holds a lease on a task of job J
    When the operator cancels job J
    Then W's next heartbeat is refused as lease lost
    And that attempt's outcome is "revoked"
    And no task of job J can be claimed

  Scenario: Delivered tiles are kept
    Given job J has delivered tile "+47-123"
    When the operator cancels job J
    Then "+47-123" remains in the target root

  Scenario: Cancel is final
    Given job J for region "PNW" revision 2 is cancelled
    When the same specification is submitted again
    Then the response is "created: false" for job J
    And job J is still cancelled
```

### 8. Retry — ruled: a new task for the same tile

**Ruling:** failed tasks are recoverable, but a retry never reopens the
abandoned task. It creates a **new task** for the same tile and task type,
and the abandoned one stays closed. `Abandoned` therefore remains terminal
as the job-server design says, and each task's attempt history stays
whole and immutable.

What this changes in the job-server design (an amendment, recorded there
when built):

- **Uniqueness.** `UNIQUE (job_id, tile, task_type)` becomes "at most one
  task per `(job_id, tile, task_type)` that is not abandoned", a partial
  unique index. A tile can therefore have one live task and any number of
  abandoned predecessors, but never two live tasks.
- **Lineage.** The new task records `retry_of` (the abandoned task's id),
  so the UI can show a tile's chain of tasks.
- **Completion gate.** The gate reads only the **current** task of each
  `(tile, task_type)`, which is the newest. A job whose abandoned tasks
  have all been retried and have succeeded is `complete`, not `failed`.
  Counts in gaps 10–12 are of current tasks, with superseded ones
  reported separately.
- **Budget.** The new task starts at zero attempts against the job's
  `max_attempts`, which was snapshotted at submission.

```
POST /api/v1/jobs/{id}/retry   body: { "task_ids": [...] }    -- per task or bulk
POST /api/v1/jobs/{id}/retry   body: { "all_failed": true }   -- every current abandoned task
→ { "created": [ { "task_id", "retry_of", "tile", "task_type" } ],
    "skipped": [ { "task_id", "reason" } ] }
```

An id that is not abandoned, or that already has a live successor, is
reported as skipped, not as an error. Retrying the same task twice
therefore creates one new task.

```gherkin
Feature: Retry failed tasks
  Scenario: Retry creates a new task for the same tile
    Given ortho task T for tile "+47-123" of job J is abandoned
    When the operator retries task T
    Then a new pending ortho task for tile "+47-123" exists, claimable now
    And it records that it retries task T
    And task T is still abandoned with its attempt history intact

  Scenario: A retried tile completes the job
    Given job J's only abandoned task T has been retried as task T2
    When task T2 succeeds
    Then job J is complete

  Scenario: Retry is idempotent per task
    Given task T has been retried as task T2, which is still pending
    When the operator retries task T again
    Then no new task is created
    And task T is reported as skipped

  Scenario: Retry all failed
    Given job J has 4 abandoned tasks and 10 pending tasks
    When the operator retries all failed tasks of job J
    Then 4 new pending tasks are created
    And job J is in progress

  Scenario: Retrying a cancelled job is refused
    Given job J is cancelled
    When the operator retries all failed tasks of job J
    Then the request is refused as a conflict
```

## Job and task reads

### 9. Job list — correction and gap

`GET /api/v1/jobs` exists only as the identity lookup. Proposal: keep the
lookup when both query parameters are present, and otherwise return a
list. Each row: `job_id, name, region_code, revision, control, status,
counts, created_at, started_at, updated_at`.

This needs the spec stored: add `jobs.name` and `jobs.spec_toml` (the
canonical TOML, which also backs "Download TOML" for an existing job and
the job header's target root and overlay setting). `started_at` comes from
the first attempt and `updated_at` from the latest task transition.

### 10. Status by task type — confirmed gap

`JobStatus` and `Throughput` count all tasks together. Proposal: one
`GET /api/v1/jobs/{id}/summary` returning `{ control, status, total,
by_type: { ortho: {pending, claimable_now, claimed, succeeded, abandoned},
overlay: {...} }, started_at, updated_at, eta }`. `eta` is
`null` until enough attempts have succeeded to estimate from. This also
settles the job-server design's open "throughput metric set" question,
which was deferred to the first real consumer — this UI.

### 11. Map data — confirmed gap

```
GET /api/v1/jobs/{id}/tiles
→ { "tiles": [ ["+47-123", "succeeded", "succeeded"], ["+47-122", "claimed", null], ... ] }
```

One array per tile: `[tile, ortho_state, overlay_state | null]`, each
the state of that tile's **current** task (gap 8: a retried tile shows its
newest task, not the abandoned one). At about
35 bytes a tile, the 1,817-tile `NA-USA-MX-CENTRAL` is about 64 KB before
compression. Serve it with an `ETag` so polling is cheap.

### 12. Task list — gap, plus two corrections

- `last_failure` **is** persisted from `POST /fail`. Expose it as
  `last_error`, and have the reaper and cancel write their own reasons.
- `max_attempts` is a job-level value, so join it in rather than storing
  it per task.
- `claimed_by` is only set while claimed. "Worker" on a finished task
  comes from its latest attempt (gap 2).
- Add `tasks.updated_at`, set on every transition. It is the sort key.

```
GET /api/v1/jobs/{id}/tasks?status=&type=&worker=&tile=&q=&cursor=&limit=
```

Use keyset pagination on `(updated_at DESC, id)`, with failed tasks pinned
by the client (one request with `status=abandoned`, then the rest). `q`
matches tile, task id prefix, or `last_error` text. CSV export is the same
query with `Accept: text/csv`.

### 13. Live updates — decision: poll

Poll. Task state changes on a timescale of minutes, heartbeats every 30 s,
and the summary, tiles and workers endpoints are small and cacheable with
`ETag`. Polling the visible view every 10 s gives an "updated n s ago" and
live heartbeat bars good to the heartbeat interval, with no connection
state in `oxo-controld`. SSE can come later for the busiest view without
changing a resource shape.

## Job creation

### 14. Validate without submitting — confirmed gap

Today an invalid spec returns `422` with every fault joined into one
string. Proposal:

```
POST /api/v1/specs/validate   (TOML body)
→ { "valid": false,
    "faults":   [ { "kind": "malformed_tile", "value": "+91+000", "message": "..." }, ... ],
    "warnings": [ ... ],          -- conflicts, gap 15
    "canonical_toml": "..." }     -- when valid, gap 17

POST /api/v1/tiles/parse      (text/plain, one tile per line)
→ { "tiles": [...], "errors": [ { "line": 7, "text": "+47-1x3", "message": "..." } ] }
```

The fault kinds are `oxo_spec::ValidationError`'s variants, serialized,
rather than parsed back out of the message text.

### 15. Conflict check — ruled: overlap is only ever a warning

**Ruling:** tiles shared between jobs are only ever a **warning**, never a
blocking fault, including when the jobs also share a target root.

Nothing in the model or the store forbids the overlap today, so this rules
out adding a block rather than lifting one. The validate response (gap 14)
lists each overlapping tile with the active jobs that hold it, and flags
the riskier case of a shared target root. There, both jobs write the same
`zOrtho4XP_<tile>` directory, and because egress is copy-then-rename, the
last writer wins.

```gherkin
Feature: Overlapping jobs
  Scenario: Overlap is a warning
    Given active job J1 includes tile "+47-123"
    When a specification with tile "+47-123" is validated
    Then validation succeeds
    And it warns that tile "+47-123" is also in job J1

  Scenario: A shared target root is called out, not refused
    Given active job J1 includes tile "+47-123" with target root "/srv/oxo/artifacts/PNW"
    When a specification with tile "+47-123" and the same target root is validated
    Then validation succeeds
    And the warning says both jobs write tile "+47-123" to the same target root
```

### 16. Advanced settings — mapping and validation

Provider and zoom are curated fields. Everything else is a `raw` key: an
Ortho4XP tile-level variable, with its value as a string.

| UI label | Spec key | Ortho4XP type and range |
|---|---|---|
| Provider | `parameters.provider` (curated) | string; validity depends on the installation's `Providers/` |
| ZL | `parameters.zoom` (curated) | integer |
| Airport cover | `raw.cover_airports_with_highres` | `False` / `True` / `ICAO` / `Existing` |
| Airport cover ZL | `raw.cover_zl` | int, default 18 |
| Airport cover radius | `raw.cover_extent` | float, **km past the airport boundary** — not a radius; relabel "Cover extent (km)" |
| Curvature tolerance | `raw.curvature_tol` | float, default 2.0 |
| Min angle | `raw.min_angle` | float (degrees), default 10.0 |
| Mesh ZL | `raw.mesh_zl` | int, one of 16–20 |
| DEM source | `raw.custom_dem` (+ `raw.fill_nodata`) | **a file path inside the worker pod**, empty = default; not something a browser can pick |
| Mask ZL | `raw.mask_zl` | int, one of 14–16 |
| Masks width | `raw.masks_width` | list as a Python literal, e.g. `[10,20,30]` (metres) |
| Water smoothing | `raw.water_smoothing` | int (passes), default 10 |
| Sea smoothing | `raw.sea_smoothing_mode` | `zero` / `mean` / `none` |

Validation rules: the spec model checks raw keys and values for syntax
only (no `=`, newlines and the like), and rejects keys that collide with
curated fields. Type, enum and existence checks happen in the worker's
runner at the `configure` phase, after a task is claimed. A bad
`mask_zl` therefore burns an attempt per task across the whole region.
Proposal: generate a **key catalogue** from the pinned Ortho4XP's
`O4_Cfg_Vars.py` (name, type, default, allowed values, hint; tile-level
only, app-level excluded) and ship it with the control plane. Serve it as
`GET /api/v1/ortho4xp/settings` so the form is built from it rather than
hand-copied, and apply it in submission-time environmental validation.

### 17. TOML round-trip — server renders

The server should render. `RegionSpec` already has one canonical
serialization (`oxo-spec show`); a client renderer would be a second one
to keep in step. The client posts the form as TOML, or as JSON with the
same shape, to `/specs/validate`, and gets `canonical_toml` back for
download.

## Other

### 18. Health — confirmed gap

`GET /healthz` (process up, no dependencies) and `GET /readyz` (database
reachable, migrations applied). The "controld connected" indicator uses
`/readyz`.

### 19. Authentication — ruled: a bearer token on every state change

**Ruling:** authentication is a new concept for OXO. A bearer token is
required for **any operation that changes production state**.

Today that means:

| Endpoint | Changes state | Token |
|---|---|---|
| `POST /jobs` (submit) | yes | required |
| `POST /jobs/{id}/pause`, `/resume`, `/cancel`, `/retry` | yes | required |
| `POST /claims` | yes (a claim takes a lease) | required |
| `POST /tasks/{id}/heartbeat`, `/complete`, `/fail` | yes | required |
| `POST /specs/validate`, `POST /tiles/parse` | no | open |
| every `GET`, `/healthz`, `/readyz` | no | open |

The worker endpoints change state too, so workers carry a token. The
proposal is two token roles: **operator** (submit and job control) and
**worker** (claim and report). A leaked worker token then cannot cancel a
region, and an operator can rotate worker tokens without touching their
own. Tokens reach workers through the pod spec's secret mechanism (a
Podman or Kubernetes secret as an environment variable), and reach
`oxo-controld` through configuration. Reads stay open on the trusted
network. Open questions for the design: whether reads should require a
token too, and whether tokens are static configuration or issued by
`oxo-controld`.

This supersedes the control-plane design's open decision ("Worker
authentication: a bearer token … is the likely v2 shape"), which is
answered here for v1.

```gherkin
Feature: Authentication on state changes
  Scenario: A state change without a token is refused
    When a job is submitted without a bearer token
    Then the response is 401 Unauthorized
    And no job is created

  Scenario: A worker token cannot control jobs
    Given a valid worker token
    When it is used to cancel job J
    Then the response is 403 Forbidden
    And job J is not cancelled

  Scenario: A worker claims with its token
    Given a valid worker token
    When a worker claims with it
    Then the claim is served

  Scenario: Reads stay open
    When job J's status is requested without a token
    Then the status is returned
```

### 20. Where the UI is served — ruled: an independent `oxo-console`

**Ruling:** `oxo-controld` serves only the API. The web interface is an
independent server process, **`oxo-console`**, which may run on the same
node as `oxo-controld` but is deployed, started and versioned on its own.

Consequences for the design:

- `oxo-console` serves the static UI and calls the `oxo-controld` API, so
  the API has to support a cross-origin browser client (CORS for the
  console's origin), or `oxo-console` proxies `/api` to `oxo-controld`. A
  proxy keeps the browser same-origin and keeps the operator token out of
  browser-to-controld traffic. Recommendation: proxy.
- The operator bearer token (gap 19) is held by `oxo-console`'s
  configuration or entered by the operator. Which one is a sub-project 5
  decision.
- `oxo-console` becomes a new workspace member (a binary crate) with its
  own Makefile targets and pod spec. It holds no persistence and no
  `TaskStore`; it reaches state only through the API, as workers do.
- `/healthz` and `/readyz` (gap 18) belong to `oxo-controld`. The console's
  "controld connected" indicator polls them through the console.

## Phasing

| Phase | Gaps | What it unlocks |
|---|---|---|
| **A — read-only console** | 2 attempts, 1 last-seen, 3 per-boot identity, 4 thresholds, 5 workers reads, 9 list (+ stored spec), 10 summary, 11 tiles, 12 tasks, 13 polling, 18 health, 20 `oxo-console` | Dashboard, both job-detail views, Workers |
| **B — authentication** | 19 | Bearer tokens on every state change, including job submission and the worker endpoints that exist today |
| **C — job control** | 6 pause, 7 cancel, 8 retry | Every action button |
| **D — job creation** | 14 validate and parse, 15 overlap warnings, 16 catalogue, 17 render | Create job, import, overlap warnings, Download TOML |
| **Later** | SSE; attempt retention with job deletion; ETA refinement | — |

Phase A goes first because gap 2 (attempts) underlies the worker column,
earlier-attempt errors, `started_at` and the ETA, and because a read-only
console is safe before authentication exists. Authentication comes before
job control: the ruling covers every state change, so pause, cancel and
retry must not ship without it. It also closes the gap on job submission
and the worker endpoints, which are open today.

## Decisions

Ruled by Sam, 2026-10-04.

| # | Question | Ruling |
|---|---|---|
| 1 | In-flight leases on cancel | Cancelling a job invalidates every associated lease (gap 7) |
| 2 | Is cancel final? | Yes. Resubmission returns the cancelled job; produce the region again with a new revision (gap 7) |
| 3 | Retry | Tasks are recoverable, but a retry creates a **new task** for the same tile; the closed task stays closed (gap 8) |
| 4 | Overlapping tiles between jobs | Only ever a warning (gap 15) |
| 5 | Worker identity | A per-boot unique identifier, used as a suffix or as the identity depending on the runtime (gap 3) |
| 6 | Authentication | New concept: a bearer token is required for any operation that changes production state (gap 19) |
| 7 | Serving the UI | `oxo-controld` serves only the API; the web interface is an independent server process, `oxo-console`, which may run on the same node (gap 20) |

Ruling 7 was given as "`oxo-controld` should not serve the ui, it should
only server the UI". It is read here as "only serve the **API**",
consistent with its second sentence.

Documents to amend when this is built: the architecture design (a new
component, `oxo-console`, and authentication in the component
boundaries), the job-server design (retry lineage, the uniqueness
constraint and the completion gate; cancelled as a job state), the
control-plane design (authentication, the read port, the new endpoints),
and the worker-pod design (per-boot identity, the worker token).

