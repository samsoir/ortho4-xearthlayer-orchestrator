# Worker Conventions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apply the operator's four production conventions to the worker pod: `skip_converts` as a region-level field defaulting to true, the deliverable narrowed to the XEL tile (DSF + `.ter` + mask `.png`s), patches always-on at pod level in Ortho4XP's flat layout, and an honest home for app-level Ortho4XP variables (refused in `raw`; OXO invariants runner-set; operational tuning via a pod-level overrides env applied through each variable's module binding).

**Architecture:** Follow-up branch to the merged sub-project 4. The region spec swaps its `patches` selector for `skip_converts: bool` (default true); the payload moves to `v: 2` carrying it; the worker's exec layer loses the per-task patches machinery and gains a filtered egress; the runner applies app-level variables to their owning modules (per `cfg_app_vars`' `module` bindings, the same aliases Ortho4XP.py uses) and refuses app-level keys smuggled into `raw`; the pod spec mounts the flat patches tree read-only at the image's fixed `Patches` location and carries the operator's operational overrides.

**Tech Stack:** unchanged from sub-project 4.

**Spec:** `docs/specs/2026-10-02-worker-pod-design.md`, specifically its "Amendments: the operator's conventions (2026-10-02)" section and the amended Decisions rows. The measured input is `docs/specs/2026-10-02-ortho4xp-pod-contract.md`; **Task 1 appends the skip_converts deliverable inventory to it**, which Task 4's egress filter consumes.

## Global Constraints

Every task's requirements implicitly include this section.

- **TDD is mandatory** for all Rust and runner-contract code: failing test first, real RED captured, minimal code to green. Behaviour already shipping gets a bite check. Tests and implementation written in one pass cost this branch's predecessor four fix rounds; do not repeat it.
- **`make verify` before every commit**; Task 2 and Task 3 touch crates the conformance/acceptance suites cover — their full gates are `make verify` only (no adapter behaviour changes anywhere in this plan; `make verify-db` is run once by the controller before the PR).
- **Payloads change shape: `v` becomes `2`.** Pre-production ruling: no deployed store holds v1 payloads, so no migration or dual-read — the version bump is honesty, not compatibility machinery.
- **The implementer sandbox cannot run podman.** Tasks needing containers (Task 1's probe; the final smoke) are controller-executed; everything else must gate on host-side tests only.
- **Commit messages end with** `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`, preceded by a blank line. Verify with `git log -1 --format='%(trailers)'`; `git status --porcelain` empty after.
- **Do not state derived counts in prose**; name the things.

### Vocabulary

As in the sub-project 4 plan. The **module map** is the alias table Ortho4XP.py itself uses for app-variable owners: `UI`→`O4_UI_Utils`, `OSM`→`O4_OSM_Utils`, `IMG`→`O4_Imagery_Utils`, `TILE`→`O4_Tile_Utils`, `OVL`→`O4_Overlay_Utils`, `CFG`→`O4_Config_Utils` (read `cfg_app_vars` at the pin for the authoritative `module` values before implementing; do not trust this list over the source).

### The payload schema v2 (normative for Tasks 3 and 5)

```json
{"v":2,"provider":"GO2","zoom":16,"raw":{"<tile key>":"<string>"},"target_root":"/srv/oxo/artifacts/NA","skip_converts":true}
```

Field order exactly as shown; `raw` always present; `skip_converts` always present (the spec default has already been applied by the time the payload is composed); `patches` is gone. One line of compact JSON, byte-deterministic.

---

### Task 1 (controller-executed): the skip_converts deliverable inventory

**Files:**
- Modify: `docs/specs/2026-10-02-ortho4xp-pod-contract.md` (append an addendum section)

**Interfaces:**
- Produces: the pod-contract addendum **"Addendum: the deliverable under skip_converts (2026-10-02)"** recording, from a real probe run: the complete file inventory of `zOrtho4XP_+51+000/` after a `build_tile` with `skip_converts=True` (names, extensions, byte counts per class), explicitly separating **ship** (the DSF under `Earth nav data/`, `terrain/*.ter`, the mask `.png`s — stating exactly where the masks live) from **perish** (jpegs, `Data*` intermediates, the tile cfg, anything else observed). Task 4 consumes the ship-set *by this section's name*.
- Consumes: the preserved spike scratch at `/tmp/oxo-spike` (run-2 outputs and cached `Orthophotos/` jpegs; warm DEM), the `oxo-worker:dev` image.

- [ ] **Step 1:** Fresh container over the preserved volumes (same mounts as the spike; recreate the container, not the data). Remove only the previous `build_tile` products that a re-run regenerates (the existing `textures/*.dds`, `Earth nav data/`, `terrain/` inside the build dir — keep the mesh/poly/alt intermediates and `Orthophotos/`), then run a probe that performs the provider init sequence, sets `TILE.skip_converts = True` on its owning module, constructs the tile (`GO2`/ZL16, same raw values as the spike where relevant) and calls `TILE.build_tile(tile)` only. Capture timing.
- [ ] **Step 2:** Inventory the build directory (`find -type f` with sizes, grouped by extension/location) and `Orthophotos/`. Identify the mask `.png`s' actual location (`textures/`? elsewhere?) — this is the fact the egress filter needs.
- [ ] **Step 3:** Append the addendum (every number transcribed from the captured output), tear down the probe container (keep `/tmp/oxo-spike` until the branch finishes), commit:

```bash
git add docs/specs/2026-10-02-ortho4xp-pod-contract.md
git commit -m "docs(spec): the deliverable under skip_converts, measured

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `oxo-spec` — `skip_converts` in, the patches selector out

**Files:**
- Modify: `oxo-spec/src/parameters.rs`
- Modify: `oxo-spec/src/validate.rs`

**Interfaces:**
- Produces: `ProductionParameters.skip_converts: bool` with `#[serde(default = "default_true")]` and `fn default_true() -> bool { true }` beside the struct, placed with the scalars before `raw`; serialisation emits it always (no skip_serializing_if — the value is meaningful in both states). `ProductionParameters.patches` is **removed**, along with `validate_patches_selector`, its four `ValidationError` variants and their tests (the variant-exhaustiveness tests shrink accordingly).
- Consumes: nothing new.

- [ ] **Step 1:** Failing tests first: absent `skip_converts` parses to `true` (every existing spec stays valid *and* gets the XEL invariant); explicit `false` round-trips; the TOML emission places it before `[raw]`; a spec text still carrying `patches = "x"` is now **rejected** by `deny_unknown_fields` (test that, with the message naming the field — the operator should hear loudly that the selector is gone). RED captured, implement, GREEN.
- [ ] **Step 2:** Remove the selector: field, validation fn, variants, their message/display tests. The compiler and the existing exhaustiveness tests are the worklist.
- [ ] **Step 3:** `make verify` green (note: `oxo-control` and `oxo-worker` consume these types — if their compile breaks on the removed field, this task fixes ONLY construction-site literals (`skip_converts: true` / removing `patches: None`), leaving semantic changes to Tasks 3–5; say in the commit what was touched there and why). Commit:

```bash
git add -A
git commit -m "feat(spec): skip_converts with a true default; the patches selector retires

The XEL invariant with a per-region escape hatch, per the operator's
conventions; patches are always-on at pod level now, so the selector
and its validation go. A spec still naming patches is rejected loudly
by deny_unknown_fields rather than silently ignored.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: `oxo-control` — payload v2

**Files:**
- Modify: `oxo-control/src/planner.rs`
- Modify: `oxo-control/src/api/tasks.rs` and/or `oxo-control/src/api/jobs.rs` (tests)
- Modify: `oxo-control/src/api/test_support.rs` (if SPEC constants need the new field exercised)

**Interfaces:**
- Produces: the v2 payload per the plan header — `v: 2`, `skip_converts` after `target_root`, `patches` gone. Byte-exact planner tests updated for: default (absent in TOML → `"skip_converts":true`) and explicit-false specs.
- Consumes: Task 2's spec field.

- [ ] **Step 1:** Failing tests: the two byte-exact payload strings (true-default and explicit-false); the claim-handler `config` assertion gains `"skip_converts": true`. RED, implement, GREEN.
- [ ] **Step 2:** `make verify` green; commit:

```bash
git add oxo-control/src
git commit -m "feat(control): payload v2 — skip_converts travels, patches does not

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: `oxo-worker` exec — filtered egress; the patches machinery retires

**Files:**
- Modify: `oxo-worker/src/exec.rs`
- Modify: `oxo-worker/src/run.rs` (prepare call-site signature)

**Interfaces:**
- Produces: ortho egress ships ONLY the ship-set from the pod-contract addendum (Task 1): `Earth nav data/**`, `terrain/**`, and the mask `.png`s from their recorded location — preserving relative layout under `target_root/zOrtho4XP_<tile>/`, still copy-to-temp-then-rename. Overlay egress unchanged. `prepare` loses the patches-link parameter and machinery (`set_patches_link`, `BadPatchesSet`, the swap/stress/escape tests); target-writability and block-dir pre-create remain.
- Consumes: the addendum's ship-set.

- [ ] **Step 1:** Failing tests: fabricated build trees now include contaminants (jpegs in `textures/`, `Data+51+000.mesh`, the tile cfg) — egress tests assert the ship-set arrives AND each contaminant class does **not** (absence assertions per class, not a directory diff); the mask `.png` from the recorded location arrives. The replacement/stale-final and crash-window tests keep their teeth against the filtered copy. RED, implement, GREEN.
- [ ] **Step 2:** Remove the patches machinery; the compiler worklist covers run.rs's call site and the tests to delete.
- [ ] **Step 3:** `make verify` green; commit:

```bash
git add oxo-worker/src
git commit -m "feat(worker): egress ships the XEL tile; per-task patches machinery retires

DSF, terrain descriptors and mask PNGs travel; jpegs, intermediates
and the tile cfg perish with scratch, per the operator's conventions
and the measured addendum. Patches are the pod's now, not the task's.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: the runner honors app-level variables; the worker carries overrides

**Files:**
- Modify: `worker/oxo_o4_runner.py`
- Modify: `oxo-worker/src/runner.rs` (RunnerInput)
- Modify: `oxo-worker/src/config.rs`, `oxo-worker/src/run.rs` (threading)
- Modify: `oxo-worker/tests/runner_contract.rs` + `oxo-worker/tests/fixtures/fake_o4/` 

**Interfaces:**
- Produces:
  - `RunnerInput` gains `app_overrides: BTreeMap<String, String>` (may be empty; strings end to end, deterministic order, converted by the runner exactly like `raw`), from a new worker flag `--o4-app-overrides` / env `OXO_O4_APP_OVERRIDES` (a JSON object of app-var names to **string** values, parsed and shape-validated at startup — a non-object or non-string value refuses startup like the other config guards).
  - Runner behaviour, in order, before any build: (1) `skip_converts` from `config` applied to its owning module per the module map; (2) each `app_overrides` entry: must be a known `cfg_app_vars` key (unknown → failed line, phase `configure`) — converted through the variable's declared type exactly like `raw`, then set on its owning module via the module map; `custom_overlay_src`/`custom_overlay_src_alternate` in overrides are refused (the pod wires those; phase `configure`); (3) `raw` keys that are app-level (`k in cfg_app_vars`) → failed line, phase `configure`, naming the key and saying app-level keys belong in the pod's overrides — closing the silent no-op.
  - Patch presence: before building, the runner logs to stderr one line — `patches: present for <tile>` / `patches: none for <tile>` — from the fixed `Patches/<tile>` path. Asserted in a contract test via captured stderr (both states).
- Consumes: payload v2 (`skip_converts`), the fake tree (gains `cfg_app_vars` entries incl. module bindings for the keys the tests use, and per-module recording so ownership is assertable — a value set on the WRONG module must fail the test).

- [ ] **Step 1:** Failing contract tests: default true reaches the owning module; explicit false reaches it; an app override (e.g. `max_download_slots` as `"2"`) lands typed on ITS owning module and not the tile; unknown override key → configure failure; `custom_overlay_src` in overrides → configure failure; app-level key in `raw` → configure failure naming the key; patch presence log both ways. RED on the current runner for each (these are new behaviours — real RED, not compile-RED where avoidable), implement, GREEN.
- [ ] **Step 2:** Worker-side: flag + startup validation (tests per config.rs's pattern), threading into RunnerInput (run.rs), run-loop tests adjusted only where RunnerInput construction changed.
- [ ] **Step 3:** `make verify` green; commit:

```bash
git add worker/oxo_o4_runner.py oxo-worker
git commit -m "feat(worker): app-level Ortho4XP variables get an honest home

skip_converts and pod-level overrides apply to each variable's owning
module (the silent tile-setattr no-op is now a loud refusal), the
overlay source stays the pod's alone, and patch presence is logged
per task so silent skipping can never recur.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: pod spec, README, example

**Files:**
- Modify: `deploy/worker-pod.yaml`
- Modify: `worker/README.md`

**Interfaces:**
- Produces: the pod spec gains the patches mount — hostPath (EDIT ME) → `/var/oxo/patches-active`, readOnly — beside the existing volumes, with a comment: flat per-tile layout, Ortho4XP's own, matched by coordinate, always on; and `OXO_O4_APP_OVERRIDES` env carrying the operator's production values as a JSON string: `{"max_download_slots":"2","max_convert_slots":"8","http_timeout":"10.0","max_connect_retries":"5","max_baddata_retries":"5","ovl_exclude_pol":"[0]","ovl_exclude_net":"[]"}` with a comment citing the design's app-variables decision. README: the patches section rewritten (always-on, flat, presence logged), the overrides knob documented, the deliverable description updated (XEL tile; perishables).
- Consumes: Tasks 4–5 shapes.

- [ ] **Step 1:** Edit both; YAML parses (`python3 -c "import yaml,sys; yaml.safe_load(open('deploy/worker-pod.yaml'))"` or equivalent — the controller re-plays it for real after the branch).
- [ ] **Step 2:** Commit:

```bash
git add deploy/worker-pod.yaml worker/README.md
git commit -m "docs(deploy): always-on patches mount and the operational overrides

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: documentation true-up

**Files:**
- Modify: `CLAUDE.md`

**Interfaces:** prose only.

- [ ] **Step 1:** Re-transcribe "Currently passing" from fresh `make verify` (run it; the controller runs `make verify-db` separately before the PR — cite its result from the controller when provided, or state the suites by name); check the worker-related lines for anything Tasks 2–6 made stale (the patches selector is gone; the deliverable changed).
- [ ] **Step 2:** Commit:

```bash
git add CLAUDE.md
git commit -m "docs: true up CLAUDE.md for the worker conventions

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
