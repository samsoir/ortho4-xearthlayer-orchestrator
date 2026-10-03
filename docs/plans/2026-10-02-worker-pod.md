# Worker Pod Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Ortho4XP worker pod — the container image, the Rust supervisor that claims and reports through the control plane's HTTP API, the Python runner that drives Ortho4XP with real exit codes, the pod spec — plus the port, wire and spec amendments that carry per-task configuration, with the long-deferred spike folded in as the opening phase.

**Architecture:** A new Rust binary crate `oxo-worker` (the supervisor) talks plain HTTP to the control plane and owns a Python subprocess (`worker/oxo_o4_runner.py`) that imports the `O4_*` modules and reports via a one-line JSON contract and real exit codes. Configuration is two-layered: topology at pod start (four mounted volumes; the image carries tools, never data), intent on the task (an opaque job-level `worker_payload` snapshotted into the task store at submission and joined into every claim — and into the `JobConflict` comparison). The spike (phase 1) builds the image and produces one real tile, recording measurements and the directory contract that later egress code depends on.

**Tech Stack:** Rust (edition 2021), `reqwest` (no TLS — v1 is plain HTTP on a trusted network), `tokio` process supervision, `rustix` for the free-space capacity check, `clap` (derive+env), Python 3 inside the image (Ortho4XP's own `requirements.txt`), podman for image work, `cucumber` 0.20.2 for acceptance.

**Spec:** `docs/specs/2026-10-02-worker-pod-design.md` (building on `docs/specs/2026-10-02-control-plane-design.md`, whose wire tests are the normative JSON contract, and `docs/specs/2026-10-01-job-server-design.md` for the port). The spike's output document `docs/specs/2026-10-02-ortho4xp-pod-contract.md` is **created by Task 2** and is a required input to Tasks 9 and 11.

## Global Constraints

Every task's requirements implicitly include this section.

- **TDD is mandatory** for all Rust and Python contract code: failing test first, real RED output captured, minimal code to green. Where a task asserts already-shipping behaviour, the RED step is a **bite check** (run once with a wrong expectation, show the failure, restore). The spike tasks (1–2) are measurement, not TDD — their evidence is recorded observations and the pod-contract document.
- **`make verify` before every commit**; tasks touching `oxo-tasks-postgres` also run `make verify-db` — except Task 3, which (like sub-project 3's Task 2) deliberately ends with the adapter uncompilable and both `verify`'s lint gate and `verify-db` red; its commit message says so, and Task 4 restores both. Tasks 3 and 4 are adjacent for exactly this reason.
- **`oxo-worker` production code depends on NO workspace crate.** It is a reference client of the HTTP contract: it owns its own request/response DTOs and parses JSON per the wire tests' shapes. Its **dev-dependencies** may (and do) use `oxo-control` + `oxo-tasks` to run the real router in-process for integration tests. `oxo-worker` never touches `sqlx` or the task store.
- **No wall-clock in tests.** Poll and heartbeat intervals are injected `Duration`s (tiny in tests); anything time-driven on the store side uses `TestClock`; container tasks (1–2, 11's smoke) are the sanctioned exception since they measure reality.
- **The worker protocol rule** (from the control plane design): any `409` on a task report means the worker has lost the task and stops; `lease_lost` vs `not_claimed` is informational only; `503` is the only retryable status; `204` on claim means no work, not an error.
- **The image carries tools, never data.** No scenery, DEM, patches or imagery in any image layer. Content reaches the pod as mounts.
- **Ortho4XP is pinned** to commit `c363134799e8130e1e897ff9064bc53d058f5ba9` — the head of `Shred86/Ortho4XP`'s `dev` branch, which is the merge of the operator's dem-crash fix (PR #90; the sibling checkout's HEAD `a2af60e` is its parent) — by Containerfile build argument, recorded as an image label. (Amended during execution: the original pin existed only in the local checkout; the operator designated Shred86 `dev` as the build target.)
- **Commit messages end with** `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`, preceded by a blank line or git parses no trailer. Verify with `git log -1 --format='%(trailers)'`.
- **After committing, confirm what you committed:** `git status --porcelain` empty, `git show HEAD:<path>` contains the change.
- **Do not state derived counts in prose**; name the things. The conformance parity test remains the one enforced count.

### Vocabulary

As before: a **job** is one submission of one specification revision; a **task** is one tile's ortho or overlay conversion; plan units are capitalised **Tasks**. The **payload** is the opaque job-level `worker_payload` string the store carries; the **config** is the same bytes parsed as JSON on the claim response. The **runner** is the Python entry point; the **supervisor** is the `oxo-worker` binary. The **install root** is the Ortho4XP checkout inside the image — `resource_path` resolves against the process's cwd, so the runner always chdirs there.

### The payload schema (normative for Tasks 3, 6, 9, 11)

```json
{
  "v": 1,
  "provider": "GO2",
  "zoom": 16,
  "raw": { "<ortho4xp tile key>": "<string value>" },
  "target_root": "/srv/oxo/artifacts/NA",
  "patches": "na-airports"
}
```

`patches` is omitted entirely when the specification has none. `raw` is
always present, possibly empty. The payload is one line of compact JSON
(no pretty-printing), composed by `oxo-control` at submission — byte-equal
across resubmissions of an unchanged spec, because the store compares it.

---

### Task 1: The image — tools, never data

**Files:**
- Create: `worker/Containerfile`
- Create: `worker/README.md`
- Modify: `Makefile`

**Interfaces:**
- Produces: `make image` → local image `oxo-worker:dev`. Inside it: the pinned Ortho4XP checkout at `/opt/ortho4xp` (the install root) with its Python dependencies installed; the fixed working directories replaced by symlinks onto the mount points below; labels `oxo.ortho4xp-commit=<pin>`.
- Mount-point contract (consumed by every later task that runs the image):

| In-container path | Volume | Replaces |
|---|---|---|
| `/var/oxo/scratch` | scratch | `tmp`, `OSM_data`, `Orthophotos`, `Masks`, `Geotiffs`, `Tiles`, `yOrtho4XP_Overlays` (each a symlink from `/opt/ortho4xp/<dir>` to `/var/oxo/scratch/<dir>`) |
| `/var/oxo/dem` | dem-cache | `Elevation_data` |
| `/var/oxo/content` | content (read-only) | consumed via config, never symlinked: the X-Plane data (`custom_overlay_src`) and `patches/<set>/…` |
| *(artifacts mount)* | artifacts | no symlink — egress is the supervisor's explicit copy, and the operator mounts it so each region's `target_root` exists |

`Patches` is also a symlink, to `/var/oxo/patches-active`, which the supervisor points at `/var/oxo/content/patches/<set>` per task (Task 9) — an indirection so the read-only content mount never needs to be writable.

- [ ] **Step 1: Containerfile**

```dockerfile
# worker/Containerfile
FROM docker.io/library/python:3.12-slim

ARG ORTHO4XP_REPO=https://github.com/Shred86/Ortho4XP.git
ARG ORTHO4XP_COMMIT=c363134799e8130e1e897ff9064bc53d058f5ba9

RUN apt-get update \
    && apt-get install -y --no-install-recommends git p7zip-full \
    && rm -rf /var/lib/apt/lists/*

RUN git clone ${ORTHO4XP_REPO} /opt/ortho4xp \
    && git -C /opt/ortho4xp checkout ${ORTHO4XP_COMMIT} \
    && rm -rf /opt/ortho4xp/.git

RUN pip install --no-cache-dir -r /opt/ortho4xp/requirements.txt

# The install-relative directories Ortho4XP writes are not configurable
# (O4_File_Names.py resolves them against the cwd), so the image bakes
# the arrangement: working dirs live on the scratch mount, elevation on
# the DEM cache, patches behind a supervisor-controlled indirection.
RUN set -eux; \
    mkdir -p /var/oxo/scratch /var/oxo/dem /var/oxo/content; \
    cd /opt/ortho4xp; \
    for d in tmp OSM_data Orthophotos Masks Geotiffs Tiles yOrtho4XP_Overlays; do \
        rm -rf "$d"; ln -s "/var/oxo/scratch/$d" "$d"; \
    done; \
    rm -rf Elevation_data; ln -s /var/oxo/dem Elevation_data; \
    rm -rf Patches; ln -s /var/oxo/patches-active Patches

LABEL oxo.ortho4xp-commit=${ORTHO4XP_COMMIT}
WORKDIR /opt/ortho4xp
```

(If the pinned checkout lacks one of the listed directories, `rm -rf` is a no-op and the symlink still lands — do not "fix" absent directories by dropping their line. If `requirements.txt` needs system packages beyond the above to install — check the pip output — add them to the single apt layer and record them in `worker/README.md`.)

- [ ] **Step 2: Makefile target + README**

Makefile (near the pg targets, same style):

```makefile
image:  ## Build the worker pod image (podman)
	podman build -t oxo-worker:dev -f worker/Containerfile .
```

`worker/README.md`: one page — what the image contains, the mount-point table above verbatim, the tools-never-data rule, and how Task 2's spike runs it. Written now because Task 2's operator needs it.

- [ ] **Step 3: Build and probe**

Run: `make image` (expect minutes on first build).
Then prove the arrangement without building anything:

```bash
podman run --rm oxo-worker:dev python3 -c "
import sys, os
sys.path.append('/opt/ortho4xp/src')
import O4_File_Names as FNAMES
print('patch_dir:', FNAMES.Patch_dir)
print('elev_dir:', FNAMES.Elevation_dir)
assert os.path.islink('/opt/ortho4xp/Elevation_data')
import O4_Imagery_Utils, O4_Vector_Map, O4_Mesh_Utils, O4_Mask_Utils, O4_Tile_Utils, O4_Overlay_Utils
print('imports ok')
"
```

Expected: the fixed dirs print under `/opt/ortho4xp/…` (cwd-resolved), the symlink assertion holds, and every `O4_*` module the runner will need imports cleanly. Capture the output — it is this Task's GREEN.

- [ ] **Step 4: Commit**

```bash
git add worker/Containerfile worker/README.md Makefile
git commit -m "feat(worker): the pod image — tools, never data

Pinned Ortho4XP checkout with its fixed working directories re-homed
onto the mount points by symlink, elevation on the shared DEM cache,
and Patches behind a supervisor-controlled indirection so the content
mount stays read-only. No scenery, DEM or patch bytes in any layer.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: The spike — one real tile, measured, and the pod-contract document

**Files:**
- Create: `docs/specs/2026-10-02-ortho4xp-pod-contract.md`

**Interfaces:**
- Produces: the pod-contract document, which MUST record, each in its own section: (a) wall-clock per phase and end-to-end for the ortho build; (b) peak memory (cgroup `memory.peak`); (c) peak scratch bytes (`du` at completion, plus the high-water mark if observable); (d) what landed in the DEM cache and the measured speedup of a second run; (e) the overlay run's timings and output location; (f) the exact directories produced for each task type and which one is the deliverable (**the directory contract** — Tasks 9 and 11 consume this section by name); (g) the probed failure taxonomy (bad provider code, unreachable network, full scratch) with each failure's observable signature from outside the process; (h) recommended defaults derived from the numbers: the worker's minimum-free-scratch threshold and an operator guideline for `--max-task-duration-secs`.
- Consumes: Task 1's image; **an operator-supplied host path to X-Plane Global Scenery / demo data** for the overlay run (the controller obtains this before dispatch; it is deployment topology and appears only in the run commands, never in committed files).

This task is measurement, not TDD. Builds run minutes-to-an-hour: use background execution and poll the container, never a foreground wait. The spike drives Ortho4XP with a throwaway probe (a python one-liner file under `/var/oxo/scratch`, passed by heredoc — it is NOT committed; the real runner is Task 11's).

- [ ] **Step 1: Volumes and a probe build (ortho, +51+000, GO2, ZL16)**

```bash
mkdir -p /tmp/oxo-spike/{scratch,dem,artifacts}
podman run -d --name oxo-spike \
  -v /tmp/oxo-spike/scratch:/var/oxo/scratch \
  -v /tmp/oxo-spike/dem:/var/oxo/dem \
  -v <OPERATOR_XPLANE_PATH>:/var/oxo/content/xplane:ro \
  oxo-worker:dev sleep infinity
podman exec oxo-spike mkdir -p /var/oxo/scratch/tmp /var/oxo/scratch/OSM_data \
  /var/oxo/scratch/Orthophotos /var/oxo/scratch/Masks /var/oxo/scratch/Geotiffs \
  /var/oxo/scratch/Tiles /var/oxo/scratch/yOrtho4XP_Overlays
```

Probe (exec'd in background; capture start/end timestamps around each phase):

```python
import sys, time
sys.path.append('/opt/ortho4xp/src')
import O4_Config_Utils as CFG
import O4_Vector_Map as VMAP, O4_Mesh_Utils as MESH
import O4_Mask_Utils as MASK, O4_Tile_Utils as TILE
tile = CFG.Tile(51, 0, '')
tile.default_website = 'GO2'; tile.default_zl = 16
for name, fn in [("build_poly_file", VMAP.build_poly_file),
                 ("build_mesh", MESH.build_mesh),
                 ("build_masks", MASK.build_masks),
                 ("build_tile", TILE.build_tile)]:
    t0 = time.time(); r = fn(tile); print(f"PHASE {name} {time.time()-t0:.1f}s -> {r}", flush=True)
```

(If `CFG.Tile`'s signature or a build function's argument differs at the pinned commit, read the source and adjust the probe — then record the correction in the pod-contract document's directory-contract section; the design document said the spike confirms the entry-point premise, and corrections are the point.)

Measure alongside: `podman exec oxo-spike cat /sys/fs/cgroup/memory.peak` (or the container's cgroup path on the host), `du -sb /tmp/oxo-spike/scratch` sampled periodically, `du -sb /tmp/oxo-spike/dem` before/after.

- [ ] **Step 2: Second ortho run (DEM/cache reuse), overlay run, failure probes**

- Wipe scratch wholesale (`rm -rf /tmp/oxo-spike/scratch/*`, recreate the subdirs), keep the DEM volume, rerun Step 1's probe: record the delta.
- Overlay probe: set `CFG.custom_overlay_src = '/var/oxo/content/xplane'` (module-level app var — confirm the exact assignment the pinned source expects), pre-create the 10° block output directory (`exist_ok=True` — the recorded TOCTOU), call `O4_Overlay_Utils.build_overlay(51, 0)`, record timing and the produced path.
- Failure probes, each from a fresh wiped scratch: provider code `NOPE` (expect an in-run failure — record what the exception/return looks like); `--network none` run (record); a near-full scratch via a small tmpfs (record). The point is each failure's *signature* — exception type and phase, or wedge — because the supervisor's reporting is designed against these.

- [ ] **Step 3: Write the pod-contract document and commit**

All sections (a)–(h) filled from the measurements — numbers transcribed from captured output, never recalled. Tear down the spike container and `/tmp/oxo-spike`.

```bash
git add docs/specs/2026-10-02-ortho4xp-pod-contract.md
git commit -m "docs(spec): the Ortho4XP pod contract, measured

Spike 0, folded into sub-project 4 as designed: one real tile
(+51+000, GO2, ZL16) built headless in the image, both task types,
with the directory contract, the failure taxonomy and the measured
numbers later tasks consume.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: The port carries the worker payload

**Files:**
- Modify: `oxo-tasks/src/request.rs`
- Modify: `oxo-tasks/src/memory.rs`
- Modify: `oxo-tasks/src/conformance.rs`

**Interfaces:**
- Produces: `CreateJob` gains `pub worker_payload: String`; `ClaimedTask` gains `pub worker_payload: String`. The store never parses it. It joins the `create_job` idempotency comparison: same identity and task set, different payload → `TaskStoreError::JobConflict`.
- Conformance cases added and registered (the parity test enforces registration): `a_claim_carries_the_jobs_worker_payload` (create with payload `"{\"v\":1}"`, claim, assert the claimed task returns it byte-equal) and `resuming_with_a_different_worker_payload_is_a_conflict` (re-create identical but payload `"{\"v\":2}"` → `JobConflict` naming the identity). Existing resume cases prove the same-payload arm once fixtures carry a payload.

**This Task ends with `make verify`'s lint gate and `make verify-db` red** (the postgres adapter no longer compiles); Task 4 restores both. Say so in the commit body, as sub-project 3's Task 2 did.

- [ ] **Step 1:** Add the two fields; the compiler's error list from `cargo test -p oxo-tasks --all-features 2>&1 | head -50` is the complete thread-through worklist (fixtures gain `worker_payload: String::new()`; the in-memory `Job` stores it; the payload joins the same-job comparison beside policy; `claim` returns it). Capture RED.
- [ ] **Step 2:** Write the two cases, register them, bite-check each (wrong expected payload / expect resume where conflict) against the in-memory adapter, restore, capture both failures and passes.
- [ ] **Step 3:** `cargo test -p oxo-tasks --all-features` green; `make format-check` and `make test-strict` green; commit:

```bash
git add oxo-tasks/src/request.rs oxo-tasks/src/memory.rs oxo-tasks/src/conformance.rs
git commit -m "feat(tasks): jobs carry an opaque worker payload

CreateJob and ClaimedTask gain worker_payload: a string the store
never interprets, snapshotted at creation, returned with every claim,
and part of the idempotency comparison — resuming a job under
silently-changed parameters is the lie JobConflict exists to prevent,
and until now that hole was open.

oxo-tasks-postgres does not compile against the new fields yet; the
next commit threads it through, so make verify's lint gate and make
verify-db are expectedly red until then.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: The PostgreSQL adapter stores the payload

**Files:**
- Create: `oxo-tasks-postgres/migrations/0002_worker_payload.sql`
- Modify: `oxo-tasks-postgres/src/lib.rs`

**Interfaces:** consumes Task 3's fields; no new surface.

- [ ] **Step 1: Migration**

```sql
-- 0002_worker_payload.sql
-- Opaque, control-plane-owned bytes delivered with every claim.
-- DEFAULT '' covers rows created before this migration; new jobs always
-- write an explicit value.
ALTER TABLE jobs ADD COLUMN worker_payload text NOT NULL DEFAULT '';
```

- [ ] **Step 2:** `cargo check -p oxo-tasks-postgres 2>&1 | head -40` is the worklist: `create_job` binds the payload on insert and includes it in the existing-job comparison exactly as the policy columns are compared; `claim`'s RETURNING joins it from `jobs`; `find_job` untouched. No other SQL changes.
- [ ] **Step 3:** `make verify-db` green (the suite now includes Task 3's cases against PostgreSQL, and the migration-idempotency test covers 0002 automatically — confirm its output names both migrations). `make verify` green (lint gate restored). Commit:

```bash
git add oxo-tasks-postgres/migrations/0002_worker_payload.sql oxo-tasks-postgres/src/lib.rs
git commit -m "feat(tasks-pg): store and serve the worker payload

Migration 0002 adds the column; create_job snapshots and compares it,
claim joins it back. make verify and make verify-db are green again.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: `oxo-spec` learns the patches selector

**Files:**
- Modify: `oxo-spec/src/parameters.rs`
- Modify: `oxo-spec/src/validate.rs`
- Modify: `oxo-spec/tests/validation.rs` (whichever suite file holds the parameter rules — read it first and follow its structure)

**Interfaces:**
- Produces: `ProductionParameters.patches: Option<String>` — `#[serde(default, skip_serializing_if = "Option::is_none")]`, placed with the scalars (before `raw`, which must stay last for TOML table ordering). Validation, in `validate_parameters`: when present it must be non-empty, a single path component (no `/`, no `\`), not `.` or `..`, and only `[A-Za-z0-9._-]`; each violation appends to the shared error list with a message naming the field and the rule, in the file's established voice.

- [ ] **Step 1:** Failing tests first: parse-with-patches round-trip; absent defaults to `None` (existing specs stay valid); each validation rule refused with its message asserted (empty, `a/b`, `..`, a character outside the set); a valid selector accepted. RED captured (missing field / missing rule), then implement, then GREEN.
- [ ] **Step 2:** `make verify` green (this touches the crate with the Gherkin suite — its scenarios must stay green untouched). Commit:

```bash
git add oxo-spec/src/parameters.rs oxo-spec/src/validate.rs oxo-spec/tests
git commit -m "feat(spec): optional patches selector on production parameters

The first amendment since the crate shipped: a validated name that
selects a subdirectory of the deployment's patches tree. Additive and
optional — every existing specification remains valid.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: The control plane composes the payload and serves the config

**Files:**
- Modify: `oxo-control/src/planner.rs`
- Modify: `oxo-control/src/api/wire.rs`
- Modify: `oxo-control/src/api/jobs.rs` (tests), `oxo-control/src/api/tasks.rs` (tests), `oxo-control/src/api/test_support.rs` (SPEC constants if needed)

**Interfaces:**
- Produces: `planner::plan` composes `worker_payload` — compact single-line JSON per the plan header's **payload schema**, keys in the fixed order shown there, `patches` omitted when `None`, `target_root` from `spec.target.root` (container path, stringified), byte-deterministic for an unchanged spec (resubmission depends on it — use `serde_json::to_string` of a struct with that field order, which serde preserves). `ClaimedTaskBody` gains `config: serde_json::Value`, parsed from the claimed task's payload; a payload that fails to parse is an `ApiError::Store(Adapter(..))`-shaped 503 — it means the store's bytes were not ours, which is an operational fault, not a client one.
- Wire tests pin the new shapes exactly, as ever.

- [ ] **Step 1:** Failing tests: planner tests assert the exact payload string for a spec with and without patches (byte equality — these two strings are the wire-level contract of Task 3's conflict comparison); the claim handler test asserts `config` as a `json!` object with the submitted spec's values; the acceptance SPEC-based tests keep passing with the new field present.
- [ ] **Step 2:** Implement; `cargo test -p oxo-control` green; `make verify` green. Commit:

```bash
git add oxo-control/src
git commit -m "feat(control): compose the worker payload, serve it as claim config

plan() snapshots the region's intent into one deterministic JSON line;
the claim response parses it back as the config object the worker
builds from. Submission to claim, through the store, without the store
ever learning what a provider is.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: `oxo-worker` — configuration and the HTTP client

**Files:**
- Modify: `Cargo.toml` (members; workspace deps: `reqwest = { version = "0.12", default-features = false, features = ["json"] }`, `rustix = { version = "1", features = ["fs"] }`)
- Create: `oxo-worker/Cargo.toml`, `oxo-worker/src/main.rs` (thin), `oxo-worker/src/config.rs`, `oxo-worker/src/api.rs`

**Interfaces:**
- Produces, `config.rs` (clap derive, env fallbacks, `oxo-controld`'s style): `--control-plane-url` / `OXO_CONTROL_URL` (required); `--worker-name` / `OXO_WORKER_NAME` (default: hostname); `--mode` / `OXO_MODE` (`recycle` | `stop`, a clap `ValueEnum`, default `recycle`); `--poll-interval-secs` (default 15); `--heartbeat-interval-secs` (default 30); `--min-free-scratch-bytes` / `OXO_MIN_FREE_SCRATCH_BYTES` (default from the pod-contract document's recommendation — read section (h) and use its number, recording it in a comment that cites the document); `--install-root` (default `/opt/ortho4xp`); `--scratch-dir` (default `/var/oxo/scratch`); `--content-dir` (default `/var/oxo/content`); `--overlay-src` / `OXO_OVERLAY_SRC` (default `/var/oxo/content/xplane` — the pod-level `custom_overlay_src`, handed to every runner invocation); `--patches-link` (default `/var/oxo/patches-active`); `--runner` (default `/opt/oxo/oxo_o4_runner.py`).
- Produces, `api.rs`: a `ControlPlane` client over `reqwest` with its own DTOs mirroring the wire contract (`ClaimedTask { task_id: Uuid, job_id: Uuid, lease_token: Uuid, tile: String, task_type: String, attempt: u32, config: serde_json::Value }`, fail outcome, error body) and methods `claim(worker, task_types) -> Result<Option<ClaimedTask>, ApiFailure>`, `heartbeat(task_id, lease) -> Result<(), ApiFailure>`, `complete(..)`, `fail(.., reason)`. `ApiFailure` distinguishes `LeaseGone` (any 409 — the body's code is logged, never branched on), `Retryable` (503 and transport errors), `Fatal(status, body)` (everything else). The config tests mirror `oxo-controld`'s (defaults, required URL, mode parsing).

- [ ] **Step 1:** Crate + config, TDD as in `oxo-controld` Task 11 of the previous plan (tests: defaults, required URL, bad mode refused).
- [ ] **Step 2:** Client integration tests — this is where the real router earns its keep. Dev-deps: `oxo-control`, `oxo-tasks`, `tokio`, `serde_json`. Test harness: bind `oxo_control::api::router(store)` on an ephemeral `TcpListener`, spawn `axum::serve`, point the client at it; submit the canonical two-tile SPEC TOML over the client's own HTTP (a plain `reqwest` POST in the harness). Tests: claim returns the task with its `config` intact; 204 maps to `Ok(None)`; heartbeat/complete happy path; a 409 (drive `reap_expired` + re-claim via the shared store handle, as the control-plane tests did) maps to `LeaseGone`; a stopped server maps to `Retryable`. RED first (client absent), then implement, then GREEN.
- [ ] **Step 3:** `make verify` green; commit:

```bash
git add Cargo.toml oxo-worker
git commit -m "feat(worker): configuration and the control-plane client

A reference client of the HTTP contract: its own DTOs, no workspace
crate in the production graph, and an error split the loop can act on
— LeaseGone stops the task, Retryable waits, Fatal exits. Integration
tests run the real router in-process.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Runner contract and process supervision

**Files:**
- Create: `oxo-worker/src/runner.rs`
- Create: `oxo-worker/tests/fixtures/stub-runner.sh` (plus `stub-runner-fail.sh`, `stub-runner-hang.sh`)
- Modify: `oxo-worker/src/main.rs` (module wiring)

**Interfaces:**
- Produces: the runner I/O types (`RunnerInput { tile: String, task_type: String, config: serde_json::Value, install_root: String, overlay_src: String }` serialized to the child's stdin; `RunnerResult { outcome: ok | failed { reason, phase } }` parsed from the LAST line of stdout — Ortho4XP chatter is on stderr by contract, but tolerate noise defensively) and `run_task(cmd, input, heartbeat: impl Stream-ish) -> TaskRun` built on `tokio::process`: spawn, write stdin, wait with a kill handle. `TaskRun::kill_and_reap()` for lease loss. Exit code 0 + parseable ok-line ⇒ success; nonzero or unparseable ⇒ failure with the best available reason (the parsed one, or `"runner exited <code> without a result line"`).
- The stubs: `stub-runner.sh` echoes a valid ok line and exits 0; `-fail` emits a failed line with reason/phase and exits 3; `-hang` emits nothing and sleeps forever (killable).

- [ ] **Step 1:** TDD over the stubs: success parsed; failure parsed with reason and phase; nonzero-without-line synthesized reason; hang killed promptly via the handle (paused-time test with a real child process — the kill is real I/O, the *decision* to kill is injected, so no wall-clock assertions beyond "it returned").
- [ ] **Step 2:** `make verify` green; commit:

```bash
git add oxo-worker/src/runner.rs oxo-worker/src/main.rs oxo-worker/tests
git commit -m "feat(worker): the runner contract and process supervision

Task JSON on stdin, one JSON result line on stdout, real exit codes —
and a kill handle, because the supervisor's defining duty is working
when the build does not.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: Prepare, egress, cleanup

**Files:**
- Create: `oxo-worker/src/exec.rs`
- Modify: `oxo-worker/src/main.rs` (module wiring)

**Interfaces:**
- Produces, consuming the pod-contract document's **directory contract** section (Task 2) for the exact source paths: `prepare(task, config, paths) -> Result<PreparedTask, PrepareError>` — verifies `target_root` exists and is writable (create a probe file, delete it), repoints the patches link (`<patches-link>` → `<content>/patches/<set>`, or removes it when `patches` is absent; the link swap is `symlink` + `rename` so a crash never leaves a dangling half-state), and for overlay tasks pre-creates the 10° block directory under the overlay output location with `create_dir_all` (the recorded TOCTOU — never rely on Ortho4XP's check). `egress(task, paths) -> Result<(), EgressError>` — moves the deliverable (ortho: the tile's output directory; overlay: the produced DSF into `target_root`'s `yOrtho4XP_Overlays/Earth nav data/<block>/`, that directory also created idempotently) by copy-to-temporary-then-rename within the artifacts filesystem. `cleanup(scratch) ` — wholesale wipe of the scratch subdirectories' contents, then recreate the empty subdirectories Task 1's table names.
- Every path is injected; tests run entirely in `tempfile` trees with fabricated build outputs matching the directory contract.

- [ ] **Step 1:** TDD: unwritable target refused before any build artifact exists; patches link lands and swaps atomically (old selector → new selector; selector → none); block dir pre-created; egress moves the fabricated outputs and leaves no temporary names behind; a simulated crash between copy and rename (inject by testing the two halves) leaves the target free of final-named partials; cleanup empties but preserves the subdirectory skeleton.
- [ ] **Step 2:** `make verify` green; commit:

```bash
git add oxo-worker/src/exec.rs oxo-worker/src/main.rs
git commit -m "feat(worker): prepare, egress and wholesale cleanup

Target writability proven before the build, patches linked by atomic
swap, the overlay block directory created idempotently on both sides,
and egress that cannot leave a half-written file under a final name.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: The loop

**Files:**
- Create: `oxo-worker/src/loop.rs` (name it `run.rs` if `loop` fights the keyword in module position — it does; use `run.rs`)
- Modify: `oxo-worker/src/main.rs` (compose config → client → run)

**Interfaces:**
- Produces: `run(config, client, deps) -> ExitReason` implementing the design's loop verbatim: capacity check via `rustix::fs::statvfs` on the scratch dir (free bytes below threshold → claim `["overlay"]` only); claim; `204` → recycle sleeps the poll interval and loops, stop exits `QueueDrained`; `Fatal` → exit `Misconfigured` (exit code 2 from main); `Retryable` → sleep, retry; a task → prepare (a `PrepareError` is a `fail` report with that reason), run with heartbeats every interval raced via `select!` against the child (heartbeat `LeaseGone` → kill, skip reporting, cleanup, continue); runner ok → egress → `complete` (an egress error is a `fail` naming egress — the artifacts are not in place, so the task is not done); runner failed → `fail(reason)`; cleanup always; stop exits `TaskDone` after one task's cleanup. Intervals and the free-space probe are injected (`deps`), so tests drive everything with tiny durations and a fake probe.

- [ ] **Step 1:** Integration tests over the in-process router + stubs + temp trees (the harness from Tasks 7–9 composed): a two-task job drained to `Complete` in recycle mode by one worker (assert both artifacts egressed, scratch empty after each); stop mode exits after exactly one task; stop mode on an empty queue exits `QueueDrained`; a failing stub burns the budget to job `Failed`; the lease-loss scenario (hang stub, reap via the store handle, next heartbeat 409) kills the child, reports nothing, and the task is re-claimable; capacity fake below threshold claims overlay-only. RED first (no `run`), GREEN after.
- [ ] **Step 2:** `make verify` green; commit:

```bash
git add oxo-worker/src
git commit -m "feat(worker): the claim loop — capacity, heartbeats, modes

Pull dispatch end to end against the real router: claim, build under a
heartbeat race, egress, report honestly (an egress error is a fail,
not a complete), wipe scratch, then recycle or stop. Any 409 means the
task is no longer ours and nothing more is said about it.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 11: The real runner, into the image

**Files:**
- Create: `worker/oxo_o4_runner.py`
- Create: `oxo-worker/tests/fixtures/fake_o4/` (stub `O4_*` modules)
- Create: `oxo-worker/tests/runner_contract.rs`
- Modify: `worker/Containerfile` (copy the runner and the `oxo-worker` binary in; a build stage compiling the workspace binary)
- Modify: `Makefile` (`worker-smoke`)

**Interfaces:**
- Produces: the runner per the design's contract — reads `RunnerInput` JSON from stdin; chdirs to `install_root` (every `resource_path` resolves there); appends `src` to `sys.path`; applies the config (`CFG.Tile(lat, lon, '')`, then `default_website`, `default_zl`, each `raw` key set on the tile — exactly the application order the spike's probe validated, including any corrections the pod-contract document recorded; sets `custom_overlay_src` from `overlay_src`); ortho = the four build calls in order, overlay = the idempotent block pre-create then `build_overlay(lat, lon)`; EVERY call wrapped so an exception becomes `{"outcome":"failed","reason":"<type>: <msg>","phase":"<fn name>"}` on stdout and exit 1, success becomes `{"outcome":"ok"}` and exit 0. Chatter goes to stderr (redirect Ortho4XP's UI stream if the spike found it writes to stdout — the pod-contract document says). The tile string parses `±DD±DDD` with `FromStr`-equivalent logic mirrored from `oxo-spec`'s format.
- The fake `O4_*` tree gives the contract tests a controllable Ortho4XP: modules whose functions succeed, raise, or record their call order/arguments to a file, selected by env var.
- `runner_contract.rs` runs `python3 worker/oxo_o4_runner.py` with `install_root` pointed at the fake tree: success path (exit 0, ok line, call order recorded as `build_poly_file → build_mesh → build_masks → build_tile`); an exception in `build_mesh` (exit 1, failed line naming the phase and the exception text); overlay path (block dir created before `build_overlay` ran — the fake asserts it exists); config application (the fake records `default_website`/`default_zl`/raw keys seen). Skip with a clear message if `python3` is absent, and say in CLAUDE.md's next true-up that the worker tests want python3 (Task 13 does).
- Containerfile: a `FROM rust AS builder` stage building `oxo-worker` (release), final stage copies the binary to `/opt/oxo/oxo-worker` and the runner to `/opt/oxo/oxo_o4_runner.py`; `ENTRYPOINT ["/opt/oxo/oxo-worker"]`. `make worker-smoke`: builds the image and runs the runner-contract fake-O4 success case *inside* the container (`podman run --rm --entrypoint python3 oxo-worker:dev /opt/oxo/oxo_o4_runner.py ...` with the fake tree bind-mounted) — proving the image's python can execute the real file.

- [ ] **Step 1:** Fake tree + failing `runner_contract.rs`, RED (runner file absent), implement the runner, GREEN.
- [ ] **Step 2:** Containerfile stage + `make image` rebuild + `make worker-smoke` captured green.
- [ ] **Step 3:** `make verify` green; commit:

```bash
git add worker/oxo_o4_runner.py worker/Containerfile oxo-worker/tests Makefile
git commit -m "feat(worker): the runner — Ortho4XP driven with real exit codes

Imports the O4_* modules and wraps every build call so an exception
becomes a reason and a phase instead of Crash! and exit 0. Contract-
tested against a fake O4 tree, then proven inside the image by
worker-smoke.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 12: Acceptance

**Files:**
- Create: `oxo-worker/features/worker_pod.feature`
- Create: `oxo-worker/tests/acceptance.rs`
- Modify: `oxo-worker/Cargo.toml` (dev-dep `cucumber = "0.20.2"`, `[[test]] harness = false`)

**Interfaces:** consumes the Task 10 harness pieces; mirrors `oxo-control/tests/acceptance.rs` for the cucumber shape.

- [ ] **Step 1: Feature file**

```gherkin
Feature: A worker pod produces a region

  The control plane serves work; a pod claims, builds, delivers and
  reports — honestly, including when the build lies.

  Scenario: A worker drains a job and the gate closes
    Given a control plane holding a submitted two-tile region with overlays
    When a recycling worker runs until the queue is empty
    Then every task's artifact is delivered under the region's target root
    And the job reports complete

  Scenario: A build that keeps failing is reported, not hidden
    Given a control plane holding a submitted one-tile region with two attempts and no backoff
    When a worker whose runner always fails runs until the queue is empty
    Then the job reports failed with one abandoned task

  Scenario: A stop-mode worker performs one task and exits
    Given a control plane holding a submitted two-tile region without overlays
    When a stop-mode worker runs once
    Then exactly one task is complete and the worker has exited
```

- [ ] **Step 2:** Steps over the in-process router + stub runners + temp trees (`fail_on_skipped`, no wall clock). RED (undefined steps) → GREEN → one bite check (flip scenario 2 to expect complete, observe the failure, restore).
- [ ] **Step 3:** `make verify` green; commit:

```bash
git add oxo-worker/features oxo-worker/tests/acceptance.rs oxo-worker/Cargo.toml
git commit -m "test(worker): acceptance — a pod drains, delivers, reports

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 13: Pod spec and the documentation true-up

**Files:**
- Create: `deploy/worker-pod.yaml`
- Modify: `worker/README.md`
- Modify: `CLAUDE.md`

**Interfaces:** none new — deployment artifact and prose.

- [ ] **Step 1:** `deploy/worker-pod.yaml` — a Podman-kube pod: the `oxo-worker:dev` image, the four volume mounts per Task 1's table (hostPath placeholders the operator edits, clearly marked), env for `OXO_CONTROL_URL`, `OXO_MODE=recycle`, and the worker's other env-fallback settings left to their defaults; resource requests stated from the pod-contract document's measured numbers, cited in a comment. Validate it: `podman kube play --dry-run deploy/worker-pod.yaml` (or `play kube` against a scratch namespace and tear down — whichever this podman version supports; capture the output). `worker/README.md` gains the "run it" section: build, mount, play.
- [ ] **Step 2:** CLAUDE.md: `oxo-worker` crate row; the worker/ and deploy/ artifacts in one line each; "Currently passing" re-transcribed from fresh `make verify` + `make verify-db` runs (run them; never arithmetic); note that `oxo-worker`'s tests require `python3` on PATH; `make image` / `make worker-smoke` in the commands list.
- [ ] **Step 3:** Commit:

```bash
git add deploy/worker-pod.yaml worker/README.md CLAUDE.md
git commit -m "docs: the v1 pod spec and the CLAUDE.md true-up

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
