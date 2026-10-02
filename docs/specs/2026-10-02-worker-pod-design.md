# Worker pod design

Sub-project 4 of the Ortho4 XEarthLayer Orchestrator: the Ortho4XP worker
pod — the container image, the supervisor that claims and reports through
the control plane's API, the Python entry point that drives Ortho4XP with
real exit codes, the pod spec, and the configuration-injection surface
(the last two moved here from sub-project 3 by the recorded amendment).

Argued from `2026-10-01-oxo-architecture-design.md`. Where this document
contradicts it, the architecture document is wrong and must be amended,
not silently diverged from. The worker's wire contract is the control
plane's: the JSON shapes pinned by `oxo-control/src/api/wire.rs`'s tests
are normative.

## Goal

A pod that starts with nothing but mounts and a handful of environment
variables, claims one task at a time from the control plane, produces that
tile's ortho scenery or extracts its overlays, delivers the result to the
artifact volume, reports honestly — including when Ortho4XP lies about
failure — and then either recycles into the next claim or stops, so that
Podman keep-alive and Kubernetes Job semantics both fall out of one image.

## Scope

In: the spike (phase 1, folded in rather than run standalone); the
container image; the Rust supervisor (`oxo-worker`); the Python entry
point that imports the `O4_*` modules; heartbeating during builds; lease
loss handling; artifact egress; wholesale scratch cleanup; recycle and
stop modes; the pod spec; the configuration surfaces (pod layer and task
layer); the port and wire amendments that carry per-task configuration;
the small `oxo-spec` amendment for the patches selector.

Out: scaling automation (the throughput contract stays open for it);
operator UI (sub-project 5); packaging/publishing (the operator's,
phase 3); incremental production; a Kubernetes operator.

## Phase 1 is the spike, and its output is a document

Spike 0 (the Ortho4XP pod contract) was deferred at decomposition time and
sub-project 4 is where the bill comes due: building the worker image *is*
most of the spike's apparatus, so the spike runs as this sub-project's
first phase instead of discarding a throwaway build. Its deliverables:

- The image builds and one real tile is produced headless in a container:
  **tile `+51+000`, provider `GOO` (Google), ZL16** — ortho task and
  overlay task both, chosen by the operator so the first output is real.
- Measured: wall-clock per build phase, peak memory (cgroup), peak scratch
  bytes, what landed in the DEM cache and whether a second run reuses it.
- A probed failure taxonomy: at minimum a bad provider code, an
  unreachable network, and a full scratch volume — what each looks like
  from outside the process, so the supervisor's failure reporting is
  designed against observed behaviour rather than guesses.
- Confirmation of the directory contract (which install-relative
  directories Ortho4XP actually writes for our two task types) and of the
  entry-point premise — that `CFG.Tile` plus the four build calls and
  `build_overlay` can be driven from an import with exceptions surfacing.

The findings are recorded in
`docs/specs/2026-10-02-ortho4xp-pod-contract.md` (created by the spike
phase, not this document), and they set: the default capacity threshold,
a sane `--max-task-duration-secs` recommendation for operators, and any
corrections to this document's directory assumptions. Code written during
the spike is a first draft of the image, not throwaway.

## Shape

| Component | What it is |
|---|---|
| `oxo-worker` | New Rust binary crate: the supervisor. Claims, heartbeats, spawns the entry point, reports, egresses, cleans, recycles or stops. Talks HTTP to the control plane; never touches the task store. |
| `worker/oxo_o4_runner.py` | The Python entry point, in this repository, copied into the image. Imports the `O4_*` modules, builds one tile or extracts one overlay, emits a structured JSON result, exits with a real exit code. The thinnest layer that can own Ortho4XP's exceptions. |
| `worker/Containerfile` | The image: Python + Ortho4XP pinned to a git commit by build argument + the runner + the `oxo-worker` binary. **Tools, never data** — no scenery, no DEM, no patches in the image. |
| `deploy/worker-pod.yaml` | The pod spec, v1: a committed, documented Podman-kube-compatible YAML the operator runs. The control plane owning and *serving* it stays reserved (see Decisions). |

### Why a Rust supervisor around a Python entry point

Ortho4XP exits 0 on every failure and its bare `except:` discards the
traceback, so failure detection must come from a layer that owns the
process, not trusts it. The entry point must be Python — it imports the
`O4_*` modules; that is the architecture's recorded remedy and the
sanctioned exception to Rust-for-server-components. Everything around it
is the supervisor's job precisely because it must keep working when the
build does not: heartbeating a wedged process from inside itself is the
failure mode lease expiry exists for. The split puts each language where
it is load-bearing: Python where the imports are, Rust where the
reliability is.

## Configuration: two layers

The settled principle "configuration is injected at pod start" is hereby
sharpened, with the operator's ruling: **topology at pod start, intent on
the task.** A pod-start-only model breaks recycling — a recycled worker
claiming a different region would run stale configuration, exactly the
drift the architecture forbids. A task-only model cannot describe where
volumes are mounted. Each layer carries what cannot drift at that layer.

### Pod layer — where things are

The pod spec mounts four volumes and sets a handful of environment
variables. What backs each volume (local disk, NAS, a bucket driver) is
deployment topology OXO never sees.

| Mount | Content | Mode |
|---|---|---|
| `scratch` | Ortho4XP's working directories. Wiped wholesale after every task. | read-write, ephemeral |
| `artifacts` | The deliverable. Mounted so that each region's `target.root` exists inside the pod. | read-write, durable |
| `dem-cache` | Mounted at Ortho4XP's fixed `Elevation_data` location. The deliberate exception to statelessness. | read-write, shared |
| `content` | Read-only source material: the X-Plane Global Scenery / demo data (the overlay source) and the patches tree (`patches/<set>/<tile>/…`). | read-only, shared |

Ortho4XP's directory layout is install-relative and fixed
(`O4_File_Names.py`: `Patch_dir`, `Elevation_dir`, `OSM_dir`,
`Imagery_dir`, `tmp`, the tile and overlay output dirs — none
configurable). The image therefore bakes the arrangement: symlinks from
the install's fixed directories onto the mounts. The one Ortho4XP
variable set at pod level is `custom_overlay_src`, pointed at the content
mount's X-Plane data — it names where *this pod's* copy lives, which is
topology, identical for every task the pod will ever claim.

Supervisor environment (flags with env fallbacks, mirroring
`oxo-controld`'s convention): control plane URL; worker name (defaults to
hostname); execution mode (`recycle` | `stop`); claim poll interval;
heartbeat interval; minimum free scratch bytes below which the worker
claims overlay tasks only (the capacity check, defaulted from the spike's
numbers); the mount paths above.

### Task layer — what to build

The claim response grows a `config` object, carried from submission to
claim through the task store (next section). Its fields:

```json
{
  "v": 1,
  "provider": "GO2",
  "zoom": 16,
  "raw": { "cover_airports_with_highres": "ICAO" },
  "target_root": "/srv/oxo/artifacts/NA",
  "patches": "na-airports"
}
```

- `provider`, `zoom`, `raw` — the region's production parameters, applied
  by the runner via `CFG.Tile`.
- `target_root` — the spec's `target.root`, verbatim, under one stated
  convention: **paths in a region specification are container paths.**
  The operator mounts `artifacts` so the path exists in the pod; the
  homogeneous platform is what makes identical in-container paths
  reasonable. The supervisor verifies it exists and is writable *before*
  the build and fails the task loudly if not — not after six hours.
- `patches` — optional selector naming a subdirectory of the content
  volume's patches tree. Before the build the supervisor links the
  selected set to Ortho4XP's fixed `Patches/` location (and clears the
  link when unset). Ortho4XP then finds `Patches/<tile>/…` by its own
  `patch_dir(lat, lon)` lookup. Bytes stay on the volume; the payload
  carries only a name. Two regions may patch the same tile differently.
- `v` — the payload schema version. The payload outlives control-plane
  deployments (it is persisted), so it versions itself rather than
  borrowing the API path's version.

Not in the payload, deliberately: `include_overlays` (the task type
already encodes it), the failure policy (the store enforces it; workers
never see it), and any content bytes.

## Port amendment: the job carries an opaque worker payload

`CreateJob` gains `worker_payload: String` and `ClaimedTask` returns it.
The store never interprets it — no orthoscenery vocabulary enters
`oxo-tasks`; the control plane owns the encoding (the JSON above). It is
job-level state delivered per claim: every task in a job shares one
parameter set, so storing it per task would be N copies of the same
bytes.

The payload lives in the task store because the task store is the only
durable state OXO has: anywhere else, a control-plane restart orphans
every in-flight job's configuration.

**The payload joins the `create_job` idempotency comparison.** Same
identity and task set but a different payload is `JobConflict`, exactly
like a different failure policy — because it *is* a different job:
resuming under silently-changed parameters is the lie `JobConflict`
exists to prevent. Today that hole is open (parameters are discarded at
submission); this closes it.

Adapter work: a second migration (`0002`, `worker_payload text NOT NULL`
on `jobs`), both adapters store and return it, and the conformance suite
grows cases for: the payload round-trips through claim; a differing
payload is the conflict's third arm; an identical payload still resumes.

Wire: `POST /api/v1/jobs` composes the payload from the submitted
specification; the claim response's `config` field embeds it. The wire
tests pin the shape exactly, as they do every other body.

## `oxo-spec` amendment: the patches selector

`parameters.patches: Option<String>`, the first change to `oxo-spec`
since it shipped. Validation: when present, non-empty, a single path
component (no separators, no `..`), from a conservative character set.
Absent means no patches, which stays the common case. Additive and
optional, so every existing specification remains valid; the CLI and
serialisation pick it up through the existing derive path.

## The supervisor's loop

```
start → validate mounts → loop:
  capacity check (free scratch) → choose task-type filter
  claim
    204 → recycle: sleep poll-interval, loop; stop: exit 0 (queue drained)
    422/4xx → log, exit 2 (misconfiguration — do not spin)
    503 → sleep, retry (the one retryable status, per the worker rule)
    200 → run the task:
      verify target_root writable; link patches; (overlay) pre-create the
        10° block output directory idempotently — Ortho4XP's own check is
        the recorded TOCTOU race; never rely on it
      spawn runner with the task JSON on stdin
      every heartbeat-interval: POST heartbeat
        409 (lease_lost or not_claimed) → kill runner, skip reporting
          (the lease is gone; both spellings mean stop), cleanup, continue
      runner exit 0 → egress artifacts into target_root, POST complete
      runner exit ≠0 → POST fail with the runner's structured reason
        (fail returns requeued-or-abandoned; the worker does not care)
      cleanup: wipe scratch wholesale
  recycle → loop; stop → exit 0
```

Reporting failures honestly includes the supervisor's own: an egress
error after a successful build is a `fail` with a reason naming egress,
not a `complete` — the artifacts are not in place, so the task is not
done. Heartbeat cadence must comfortably beat the server's
`--heartbeat-timeout-secs` (default 120): the worker defaults to 30
seconds and treats a heartbeat *send* failure as retryable until the
lease answer says otherwise.

**Stop-mode semantics:** exit 0 on a drained queue (204) or after
completing a task's cleanup — one task per pod-run at most, which is what
a Kubernetes Job wants. **Recycle** polls forever. This closes the
architecture document's open decision "where execution mode is set": it
is a pod-level operational knob (an env var in the pod spec), because
with region intent on the task there is nothing regional left in it.

### The runner's contract

stdin: the task JSON (tile, task type, the config payload). stdout: a
single JSON result line (`{"outcome": "ok"}` or
`{"outcome": "failed", "reason": "...", "phase": "build_mesh"}`), with
Ortho4XP's own chatter going to stderr for the logs. Exit code 0 exactly
when the outcome is ok. Inside: `CFG.Tile(lat, lon, …)` with the payload's
values applied; ortho = `build_poly_file → build_mesh → build_masks →
build_tile`; overlay = `build_overlay(lat, lon)`; every call wrapped so
exceptions become a reason and a phase instead of `Crash!` and exit 0.
The runner never talks to the network control plane and never deletes
anything — reporting and cleanup belong to the supervisor.

### Egress

The build writes into scratch (via the install's fixed directories).
After a successful run the supervisor moves the deliverable into
`target_root`: the ortho tile's `zOrtho4XP_<tile>/` directory, or the
overlay's DSF into the shared `yOrtho4XP_Overlays/Earth nav data/<10°
block>/` tree. Moves are copy-to-temporary-then-rename within the
artifacts filesystem, so a crash mid-egress never leaves a half-written
file under a final name; the overlay block directory is created
idempotently on the artifacts side too, for the same reason as on the
scratch side. Exact source paths are confirmed by the spike before the
egress code is written.

## Testing

- **Supervisor:** unit and integration tests against a real control plane
  in-process — `oxo-control` is a library, so tests bind its router over
  the in-memory store on an ephemeral port and run the actual HTTP loop
  with a **stub runner** (a script that emits the contract's JSON and
  exit codes on cue: success, failure, hang-until-killed). Lease-loss,
  409 handling, stop-versus-recycle, capacity filtering, egress atomicity
  and cleanup are all assertable this way without Ortho4XP, a container,
  or a database. No wall-clock sleeps: the poll and heartbeat intervals
  are injected durations, tiny in tests.
- **Runner:** kept thin precisely because it can only be exercised for
  real inside the image. Its JSON/exit contract is tested from the Rust
  side via the stub equivalence; the real thing is proven by the spike
  and by a `make worker-smoke` target that runs one tiny in-container
  invocation.
- **Acceptance:** a Gherkin feature driving the full loop — operator
  submits, a worker pod (supervisor + stub runner) drains the job, the
  gate reports done; a failing runner burns the budget and the job
  reports failed.
- **Conformance:** the payload cases run against both adapters under the
  existing `make verify` / `make verify-db` split.
- The image build is not part of `make verify` (it needs podman and
  minutes); `make image` builds it, and the spike phase plus
  `worker-smoke` are its proof.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Spike 0 | Folded in as phase 1, output recorded as the pod-contract document | Building the image is the spike's apparatus; a standalone spike would be thrown away. Operator-chosen fixture: `+51+000`, Google, ZL16. |
| Config transport | Topology at pod start; intent on the task, as an opaque job-level payload in the task store, delivered per claim | Pod-start-only breaks recycling (stale region config — operator ruling); the store is the only durable home; opacity keeps the port generic. |
| Payload in idempotency | A differing payload is `JobConflict` | Resuming under silently-changed parameters is the lie `JobConflict` exists to prevent; closes a real hole. |
| Image contents | Tools, never data | Global Scenery and friends are large and deployment-specific (operator ruling); everything reaches the pod as mounts whose backing store OXO never sees. |
| Supervisor language | Rust binary driving a Python runner subprocess | Ortho4XP exits 0 on failure, so the reliable layer must own the process; Python only where the `O4_*` imports force it. |
| Patches | Spec names a set; supervisor links it to the fixed `Patch_dir`; bytes on the content volume | `Patch_dir` is not configurable in Ortho4XP (verified); linking per task gives region-scoped patches without content in the store. |
| Target paths | Spec paths are container paths; worker validates writability up front | Homogeneous platform; the alternative (path translation) adds a mapping layer nobody needs yet. |
| Execution mode | Pod-level env knob: `recycle` polls, `stop` exits 0 on drain or after one task | With intent on the task, nothing regional remains in the mode; closes the architecture doc's open decision on the operational side. |
| Pod spec, v1 | A committed `deploy/worker-pod.yaml` the operator runs | The control plane *serving* pod specs adds an endpoint with one consumer and no automation to use it; committed-and-documented is the honest v1. The ownership stays with this repo either way. |
| Ortho4XP pinning | Image build argument pins a git commit; recorded as an image label | Version skew is a recorded failure mode of the manual process; the image is the unit all workers share. Asserting provenance at claim time stays open. |
| Runner I/O | Task JSON on stdin, one JSON result line on stdout, real exit codes | The smallest honest contract; stderr stays Ortho4XP's, so logs survive. |

## Open decisions

- **DEM cache concurrency.** Two pods needing the same uncached DEM may
  race the download. The spike observes Ortho4XP's download behaviour;
  until then the candidate answer is atomic rename on completion with
  redundant downloads accepted, and a bound on cache growth stays open.
- **Retry classification.** Unchanged from the architecture document: the
  spike's failure taxonomy is the input that decides whether
  transient-versus-permanent is achievable through the runner.
- **Provenance assertion.** The image label records the Ortho4XP commit;
  whether workers assert it to the control plane (and the control plane
  refuses mismatched workers) is deferred until there is more than one
  image in the wild.
- **Per-tile resource estimation.** Unchanged: the capacity check stays a
  task-type filter plus a free-scratch threshold until the spike's
  numbers exist; a footprint column and claim predicate remain additive.

## Out of scope

- Scaling actuation; the operator interface (sub-project 5); packaging
  and publishing (phase 3, the operator's); incremental production; a
  Kubernetes operator; TLS and worker authentication (the recorded v1
  posture is a trusted network).

## Rejected alternatives

**Configuration injected at pod start only.** The architecture's original
words, workable for one-shot pods. Rejected by the operator's ruling: a
recycling worker crossing regions would run stale configuration — the
exact drift the architecture forbids — and region-bound worker pools
reintroduce the scheduling OXO refuses to own.

**A typed parameters struct on the port.** Honest types end to end.
Rejected: it welds orthoscenery vocabulary into the generic job server;
every parameter change becomes a port change and a migration. The store
carries bytes; the control plane owns meaning.

**Workers fetch configuration from a control-plane endpoint per job.**
Smaller claim responses. Rejected: it adds a second request, a cache, and
a consistency question (which version did I fetch?) to save bytes that
fit in the claim response — and the control plane would have to persist
specs somewhere, which lands back in the task store anyway.

**An all-Python worker.** One language, no subprocess seam. Rejected:
heartbeating and lease handling would live in the same interpreter as a
library that wedges, swallows exceptions, and exits 0 on failure; the
supervisor exists to be the thing that survives the build.

**Baking a region's content (scenery, patches) into per-region images.**
Immutable and reproducible. Rejected outright by the operator: the data
is large, deployment-specific, and image rebuilds per region defeat the
single shared image that makes version skew unrepresentable.

**Patches shipped through the task store as content.** No content volume
needed. Rejected: the store holds state, not files; patch sets can be
large and binary; and the volume model already exists for exactly this.

## Related

- `docs/specs/2026-10-01-oxo-architecture-design.md` — the execution
  model; its open decisions on execution mode close here (operational
  side) and its configuration-injection principle is sharpened here.
- `docs/specs/2026-10-02-control-plane-design.md` — the API this worker
  consumes; the claim response's `config` field lands as the additive
  change that document anticipated.
- `docs/specs/2026-10-01-job-server-design.md` — the port receiving the
  `worker_payload` amendment.
- `docs/specs/2026-10-02-ortho4xp-pod-contract.md` — created by the spike
  phase; the measured numbers and failure taxonomy.
