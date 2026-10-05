# OXO operator interface design (sub-project 5, umbrella)

Sub-project 5 of the [architecture design](2026-10-01-oxo-architecture-design.md):
observability and the operator interface. Its scope is telemetry export,
failure policy and alerting, and operator views in HTML5/CSS/JS built to
WCAG principles.

This is the **umbrella** design. Sub-project 5 is too large for one
design-plan-implementation cycle, so it is split into five phases, 5a to
5e. This document fixes only the decisions that every phase shares: the
components, the shared data model, the API conventions, the console's
architecture, and the direction for authentication, telemetry and
alerting. Each phase then gets its own design document, plan and PR, which
add detail but may not contradict this one without amending it.

Inputs:

- [Web UI service gaps](2026-10-04-web-ui-service-gaps.md): the review of
  the Figma handoff (<https://www.figma.com/design/dJyiarI0mfqgfCVifUNlO4>)
  against the code, with Sam's seven rulings of 2026-10-04. That
  document's gap numbers (gap 1 to gap 20) are cited here, and its Gherkin
  scenarios are the starting acceptance criteria for the phases.
- The design questions settled in brainstorming on 2026-10-04, recorded
  under "Decisions" below.

## Goal

An operator can watch, control and create production through a web
console. The platform's monitoring can scrape OXO's throughput signal, and
a region's owner hears about the events that matter to that region. None
of this may turn OXO into a bespoke platform, give the console state of
its own, or let a read leak a secret.

## Components

```
 browser ──HTML/JS──▶ oxo-console ──HTTP (proxy + server-side calls)──▶ oxo-controld ──▶ PostgreSQL
                      (Rust, axum,                                       │  ├─ /api/v1 (JSON)
                       templates,                                        │  ├─ /metrics (Prometheus)
                       sessions)                                         │  └─ webhooks ──▶ alert destinations
 worker pods ──────────────────────────HTTP + worker token──────────────▶┘
 Prometheus ──scrape──▶ /metrics
```

- **`oxo-console`** is a new binary crate. It renders pages server-side by
  calling `oxo-controld`'s API, and proxies `/api/*` for its JavaScript
  islands. It holds operator sessions and nothing else: no database, no
  `TaskStore`, no copy of any job state. Like the workers, it is an API
  client. It may run on the same node as `oxo-controld`, but it is deployed,
  started and versioned independently (ruling 7).
- **`oxo-controld`** serves only the API and the operational endpoints
  (`/metrics`, `/healthz`, `/readyz`), never HTML. Sub-project 5 adds read
  endpoints, bearer-token middleware, job-control endpoints, and the alert
  sender.
- **`OperatorQueries`** is a second port in `oxo-tasks`, beside
  `TaskStore`. It answers the operator's reads (job list, summaries, tile
  states, task lists, workers, attempts), and both adapters implement it
  with their own conformance suite. Interface segregation is the reason:
  workers never need these reads, and adding them to `TaskStore` would
  make every task-store adapter carry UI concerns. State changes the
  console drives (pause, cancel, retry) carry lifecycle rules, so they
  belong on `TaskStore` and its conformance suite.

## Phases

| Phase | Builds | Depends on |
|---|---|---|
| **5a** Read-only console | attempts and workers tables; per-boot worker identity; stored spec, with alert destinations stored apart; `OperatorQueries`; read endpoints; `/healthz` and `/readyz`; `oxo-console` with Dashboard, Job detail (map and list views) and Workers | — |
| **5b** Authentication | static per-role token hashes, file-watched; the `Authenticator` trait; `oxo-controld token new`; middleware on every state change, worker endpoints included; `OXO_WORKER_TOKEN`; console sign-in | 5a |
| **5c** Job control | pause and resume; cancel (revokes leases, final); retry as a new task (partial unique index, lineage, a completion gate that reads the current task) | 5b |
| **5d** Job creation | validate and parse endpoints; overlap warnings; the Ortho4XP settings catalogue; canonical TOML rendering; the create-job screens; the tile-set authoring UX specification | 5b |
| **5e** Telemetry and alerting | `/metrics`; webhook alert destinations, the outbox and the sender; an example Alertmanager rules file | 5a |

5b precedes 5c and 5d because ruling 6 requires a bearer token for any
operation that changes production state, so no control or creation screen
may ship before authentication exists. 5e depends only on 5a and can run
in parallel with 5b to 5d.

The tile-set authoring UX specification, which the architecture design
names as a separate document belonging with the web interface, is written
as part of 5d. The Figma create-job screens (tile map, box select, `.txt`
import) are its first draft.

## Shared data model

All migrations are additive. Jobs that exist before a migration keep
working, and the columns they predate read as `null`.

**New tables (5a):**

```sql
workers  (identity        text PRIMARY KEY,
          first_seen_at   timestamptz NOT NULL,
          last_seen_at    timestamptz NOT NULL,
          last_task_types text)          -- null = any

attempts (id          uuid PRIMARY KEY,
          task_id     uuid NOT NULL REFERENCES tasks ON DELETE CASCADE,
          job_id      uuid NOT NULL REFERENCES jobs  ON DELETE CASCADE,
          worker      text NOT NULL,
          attempt_no  bigint NOT NULL,
          claimed_at  timestamptz NOT NULL,
          ended_at    timestamptz,       -- null while open
          outcome     text,              -- null while open
          reason      text)
```

`outcome` is one of `succeeded`, `failed`, `reclaimed_heartbeat`,
`reclaimed_max_duration`, `revoked` and `superseded`. An attempt opens at
claim and closes at complete, fail, reap or revoke (gap 2). Rows live as
long as their job does.

`workers` is upserted on every claim, including 204 misses, and on every
heartbeat (gap 1). The `identity` is the per-boot worker identity (gap 3):
a unique identifier generated once per process start, used as a suffix on
the configured or host name (`<name>-<boot id>`) under Podman, where one
YAML gives every pod the same hostname, or as the identity itself where
the runtime supplies no meaningful name. The wire field `worker` stays one
string.

**Changes to existing tables:**

| Column | Phase | Notes |
|---|---|---|
| `jobs.name` | 5a | from `metadata.name` |
| `jobs.spec_toml` | 5a | canonical TOML with `alert_destinations` **removed** |
| `jobs.alert_destinations` | 5a (used by 5e) | stored apart from the spec; read only by the alert sender, never returned by any endpoint |
| `tasks.updated_at` | 5a | set on every transition; the task list's sort key |
| `jobs.control` | 5c | `active`, `paused` or `cancelled` |
| `tasks.retry_of` | 5c | the abandoned task a retry replaces |
| `UNIQUE (job_id, tile, task_type)` becomes a partial unique index over non-abandoned tasks | 5c | at most one live task per tile and type; amends the job-server design |

Storing the destinations apart, rather than redacting them at read time,
means no new read endpoint can leak a webhook URL by forgetting to redact
it. The secret is not in the data a read sees.

**Job identity is unchanged:** `(region_code, revision)`. Alert
destinations are part of the snapshotted failure policy, so resubmitting a
spec whose destinations differ is `409 JobConflict`, just as a changed
retry budget is today. The resume semantics (`created: false` for an
identical resubmission) are unchanged.

## API conventions

These apply to every endpoint sub-project 5 adds:

- Everything is under `/api/v1`, and every change is additive. The
  operational endpoints `/healthz`, `/readyz` and `/metrics` sit outside it.
- Times are RFC 3339 in UTC.
- Lists use keyset pagination: `?cursor=&limit=`, with `next_cursor` in the
  response.
- Polled resources (job summary, tile states, workers) carry an `ETag` and
  answer `304 Not Modified` when unchanged.
- Errors keep the existing JSON error shape and status mapping.
- From 5b, a state change without a valid token answers `401`, and one
  with a valid token of the wrong role answers `403`. Reads stay open.

The endpoint shapes proposed in the gaps document (gaps 5, 8, 9 to 12, 14
and 18) are the starting point for each phase's design.

## The console

**Routes.** `oxo-console` is an axum binary configured with
`--controld-url`, `--bind` and a session secret. It has three kinds of
route:

| Route | Purpose |
|---|---|
| Pages: `/`, `/jobs/{id}`, `/jobs/{id}?view=list`, `/workers`; `/jobs/new` (5d) | Rendered server-side with askama templates from `oxo-controld` API calls made while handling the request |
| Fragments: `/frag/...` | HTML partials (job summary, tile grid, task rows, worker rows) that the JavaScript islands fetch on a poll, rendered by the same templates as the full pages, so a page's first render and its updates cannot drift apart |
| Proxy: `/api/*` | Forwarded to `oxo-controld`, for CSV export and anything better served as JSON. From 5b, the session's bearer token is attached to forwarded state changes |

**JavaScript islands.** Plain ES modules served by `oxo-console`, with no
framework and no build step:

- **Poller.** Swaps a fragment every 10 s while the tab is visible, backs
  off while it is hidden, uses the `ETag` so an unchanged view costs a
  `304`, and maintains "updated n s ago". Polling, not SSE, is the live
  update mechanism (gap 13).
- **Tile map.** A canvas layer over the server-rendered tile list: each
  tile's body shows the ortho task's state and a bottom strip the overlay
  task's. Clicking a tile opens the task panel. It is an enhancement only:
  the list view carries the same information without JavaScript.
- **Selection.** Bulk selection for "Retry selected" (5c), and later the
  map tile picker (5d).

**Accessibility (WCAG 2.2 AA):**

- Every read works with JavaScript disabled.
- State is never conveyed by colour alone. Each state has a text label,
  and on the map a shape or pattern as well.
- The tile map has a keyboard-navigable grid alternative.
- Fragment swaps announce through `aria-live` regions: polite for
  progress, assertive for failures.
- The type-to-confirm cancel dialog is a native `<dialog>`, and confirmation
  types the job's region code and revision.

**Sessions (5b).** Reads need no sign-in. Before the first action, the
operator pastes an operator token once. `oxo-console` keeps it server-side
in an in-memory session behind an `HttpOnly`, `SameSite=Strict` cookie
(`Secure` when served over TLS), and every form carries a CSRF token.
Until the operator signs in, action buttons render as locked and say why.
A console restart signs everyone out, which is acceptable for an operator
tool and keeps the console stateless.

## Authentication (5b)

Authentication is new to OXO. Ruling 6 requires a bearer token for any
operation that changes production state.

- **Roles.** `operator` covers submit, pause, resume, cancel and retry.
  `worker` covers claim, heartbeat, complete and fail. Each state-changing
  endpoint requires exactly one role. A valid token of the other role is
  `403`. Reads, `/metrics` and the health endpoints need no token.
- **A token belongs to a fleet, not a pod.** One worker token is shared by
  every replica of a deployment. Telling pods apart is the per-boot
  identity's job (gap 3), not authentication's.
- **Configuration.** `oxo-controld` reads
  `--operator-token-hashes-file` and `--worker-token-hashes-file`. Each
  holds one line per token: `sha256:<hex> <label>`. Revoking a token means
  deleting its line.
- **Reload.** The files are **watched** and reloaded when they change, so a
  Kubernetes Secret mounted as a volume rotates without a restart. SIGHUP
  also triggers a reload where that is convenient (Podman, systemd).
- **Minting.** `oxo-controld token new --role worker --label k8s-workers`
  prints the token once and the line to add to the file. Tokens are 32
  random bytes in base64url. Their entropy makes a plain SHA-256 hash
  sufficient, and comparison is constant-time. Minting talks to no
  server, so it runs anywhere the binary does.
- **Rotation.** Add the new hash line while the old one stays valid,
  update the token where its holders read it, restart them (for
  Kubernetes, `kubectl rollout restart`), then delete the old line. No
  holder is refused at any point.
- **Logging.** A rejected request is logged with the token's label when
  the hash matched one, and never with the token itself.
- **Workers** read `OXO_WORKER_TOKEN`, delivered through the platform's
  secret mechanism (a Podman or Kubernetes secret as an environment
  variable). A worker without one refuses to start, with a clear error,
  rather than looping on `401`.
- **No "auth off" switch.** `oxo-controld` started without token files
  accepts no state change at all, and says so loudly at startup.
- **The `Authenticator` trait.** Token verification sits behind a trait,
  and v1 ships one implementation, static hashes. Per-pod credentials on
  Kubernetes (projected service-account tokens, verified through the
  cluster's OIDC discovery keys or the TokenReview API) are a later,
  additive second implementation, not a redesign.

The 5b design includes worked examples for both Podman (secret, pod spec,
rotation) and Kubernetes (Secret, Deployment with an HPA, rotation).

## Telemetry (5e)

`oxo-controld` serves `GET /metrics` in Prometheus text format. The
initial metric families:

| Metric | Type | Labels |
|---|---|---|
| `oxo_tasks` | gauge | `job`, `task_type`, `state` (with `claimable_now` reported separately) |
| `oxo_task_transitions_total` | counter | `task_type`, `outcome` (from attempt outcomes) |
| `oxo_workers` | gauge | `state` (Working, Late, Reaping, Idle, Gone quiet) |
| `oxo_attempt_duration_seconds` | histogram | `task_type`, `outcome` |
| `oxo_reaper_reclaims_total` | counter | `reason` |

The `job` label is `<region_code>/<revision>`, bounded by active jobs. A
finished job leaves the gauges after a retention window, so label
cardinality stays bounded over time. The same signal serves dashboards,
alert rules and any future autoscaler; acting on it remains the
platform's job.

## Alerting (5e)

- **Destinations** are `https://` URLs, validated at submission. The check
  is static, so it lives in `oxo-spec` and amends the region-spec design's
  open "alert destination representation" decision.
- **Events** are the region-level facts only OXO knows: `task_abandoned`,
  `job_failed`, `job_complete` and `job_cancelled`.
- **Payload.** A small JSON object posted to each destination:
  `{event_id, event, region_code, revision, job_id, tile?, task_type?,
  reason?, at}`.
- **Delivery.** An outbox table (`alert_outbox`) is written in the same
  transaction as the transition that raises the event, and a background
  sender delivers it with retry and backoff. A slow or failing webhook
  therefore never blocks a claim or a report, and a restart loses no
  alert. Delivery is at least once; `event_id` lets a receiver
  deduplicate.
- **Infrastructure alerts are not OXO's.** "No worker seen for 10 min" or
  "queue stalled" are Alertmanager rules over `/metrics`. 5e ships an
  example rules file.

## Testing

- **Strict TDD**, as everywhere in OXO: every change starts with a failing
  test.
- **Conformance.** New `TaskStore` behaviour (pause, cancel, retry) gets
  conformance cases. `OperatorQueries` gets its own suite. Both run against
  the in-memory adapter in `make verify` and against PostgreSQL in
  `make verify-db`, keeping the existing visible split.
- **Gherkin.** The gaps document's scenarios become feature files:
  `oxo-control` for the API and lifecycle, `oxo-worker` for identity and
  the worker token, and a new `oxo-console` suite for the screens, driven
  against a real `oxo-controld` over the in-memory store.
- **Console.** Rust tests render every template from fixtures, exercise the
  fragment and proxy routes against a fake `oxo-controld`, and assert
  accessibility markup (landmarks, labels, `aria-live`) in the rendered
  HTML. The islands' logic is tested with Node's built-in test runner
  through a new `make test-js` target. Node is a development dependency
  only; nothing ships with it. Visual review against the Figma screens is
  manual.
- **Guards.** `make verify` checks that the `/metrics` output parses, and
  that no read endpoint returns a stored alert destination (a seeded
  webhook URL is searched for in every read response).

```gherkin
Feature: Operator interface boundaries
  Scenario: The console holds no state
    Given oxo-console is restarted
    Then no job, task or worker data is lost
    And signed-in operators must sign in again

  Scenario: A secret never leaves through a read
    Given a job submitted with alert destination "https://hooks.example/abc123"
    When every read endpoint and console page for that job is fetched
    Then none of the responses contains "abc123"

  Scenario: Reads work without JavaScript
    Given a browser with JavaScript disabled
    When the operator opens the dashboard, a job, and the workers page
    Then each page shows the same job, task and worker states as with JavaScript enabled

  Scenario: Every state change needs a token
    Given oxo-controld is running with token files
    When any state-changing endpoint is called without a token
    Then the response is 401 Unauthorized
    And no state has changed

  Scenario: One worker token serves a whole fleet
    Given a worker token labelled "k8s-workers"
    When three worker pods start from one deployment using that token
    Then all three can claim work
    And their reported identities differ
```

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Decomposition | Umbrella design, then phases 5a–5e with their own design, plan and PR | One sub-project per cycle is the project's convention, and sub-project 5 spans six concerns |
| Console process | Independent `oxo-console`; `oxo-controld` serves only the API | Ruling 7 |
| Console rendering | Server-rendered askama templates plus vanilla-JS islands, no build step | Pages work as documents, the strongest WCAG baseline; the UI is tested in Rust; no second toolchain at runtime |
| Live updates | Polled fragments with `ETag` | State changes over minutes; no connection state in `oxo-controld`; SSE can come later without a resource change |
| Read model | A second port, `OperatorQueries`, in both adapters | Interface segregation; keeps `oxo-control` testable without a database |
| Alert destination secrecy | Stored apart from the spec, never returned | A read cannot leak what it never sees |
| Telemetry | Prometheus `/metrics` | Pull-based, standard on Podman and Kubernetes, and the same signal a future autoscaler needs |
| Alerting | OXO sends job-event webhooks through an outbox; infrastructure alerts belong to Alertmanager | OXO alerts on region facts only it knows; the outbox decouples delivery from the claim path |
| Token provisioning | Static per-role hashes in config files, watched for changes | No token table, no issuing API, nothing to bootstrap; file watching suits Kubernetes Secrets |
| Token scope | Per fleet, not per pod | An HPA's pods cannot be provisioned by hand; identity is the per-boot id's job |
| Read access | Open | Ruling 6 covers state changes; no read exposes a secret |
| Console authentication | The operator signs in with their token; server-side session; CSRF on forms | A console holding a token itself would be an unauthenticated control surface |
| Disabling authentication | Not possible | A switch would be left on |
| Per-pod credentials | An `Authenticator` trait; service-account tokens later | Leaves room without building it |

## Open decisions

Each is settled in the phase that touches it:

- **Retry classification** (5c): whether a usable transient-versus-permanent
  distinction exists for failures, or every failure stays retryable up to
  the budget.
- **Worker liveness thresholds** (5a): the gaps document's defaults (late
  at 60 s, idle within 2 min, gone quiet after 10 min) become
  `oxo-controld` flags, and the workers endpoint returns them. Whether
  "idle but stale" is its own state is a console design call.
- **Reaper defaults** (5a): revisit the 120 s heartbeat timeout and 6 h
  maximum task duration against the measured ortho task time (6m39s at
  ZL16 on +47-123).
- **Metric retention window** (5e): how long a finished job stays in the
  gauges.
- **Provenance assertion, DEM cache concurrency, per-tile resource
  estimation and the texture-bytes floor** are open in earlier designs.
  Sub-project 5 does not settle them; the console can show them once they
  exist.

## Out of scope

- A Kubernetes operator, and scaling automation. `/metrics` exposes the
  signal; acting on it is the platform's.
- Per-pod credentials (left open by the `Authenticator` trait).
- Multi-user accounts, user-level roles and an audit UI. A token's label is
  the only actor identity, and it is logged.
- TLS termination. It belongs in front of both processes (a reverse proxy
  or ingress).
- Discord-native payloads, SSE, job deletion and attempt retention.

## Rejected alternatives

- **`oxo-controld` serving the UI.** Ruled out by ruling 7.
- **A single-page app** (vanilla or framework). It moves rendering and
  accessibility into client JavaScript, and a framework adds a Node build
  toolchain to the runtime path.
- **Reads on `TaskStore`.** Every adapter would carry UI concerns.
- **Postgres-only SQL views for reads.** The in-memory adapter could not
  serve them, so `oxo-control`'s tests would need a database.
- **An event log with projections.** A bespoke event-sourcing system, close
  to the non-goal of building a task management system.
- **OpenTelemetry (OTLP push).** Needs a collector to be running, and more
  dependency surface, for no gain over a scrape endpoint at this scale.
- **Delegating all alerting to Alertmanager.** It cannot know region-level
  facts such as "job complete" without per-job metric labels, and it would
  have meant removing `alert_destinations` from the specification.
- **Named alert channels in `oxo-controld`'s configuration.** One more
  indirection, solving a secrecy problem that storing the destinations
  apart already solves.
- **Tokens issued by `oxo-controld`.** A token table, an issuing API and a
  bootstrap token, for live per-token revocation that deleting a line from
  a watched file already gives.
- **The console holding an operator token.** It would make the console an
  unauthenticated control surface.
- **SIGHUP as the only reload mechanism.** Nothing signals a process in a
  Kubernetes pod.

## Amendments to earlier documents

| Document | Amendment | When |
|---|---|---|
| Architecture design | sub-project 5 split into 5a–5e; `oxo-console` as a component; authentication in the component boundaries | with this document |
| Region-spec design | `alert_destinations` become validated `https://` URLs | 5e |
| Job-server design | the `OperatorQueries` port (5a); retry lineage, the partial unique index, the completion gate reading the current task, and the `cancelled` job state (5c) | 5a, 5c |
| Control-plane design | the read, operational and control endpoints; authentication, which answers its bearer-token open decision | 5a–5c |
| Worker-pod design | per-boot identity (5a); `OXO_WORKER_TOKEN` (5b) | 5a, 5b |

## Related

- [Architecture design](2026-10-01-oxo-architecture-design.md)
- [Web UI service gaps](2026-10-04-web-ui-service-gaps.md)
- [Job-server design](2026-10-01-job-server-design.md)
- [Control-plane design](2026-10-02-control-plane-design.md)
- [Worker-pod design](2026-10-02-worker-pod-design.md)
- [Region-spec design](2026-10-01-region-spec-design.md)
