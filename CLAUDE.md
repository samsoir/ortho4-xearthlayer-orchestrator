# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository Status

**This repository contains no code yet.** It holds `README.md` (the high-level specification), `docs/specs/` (design documents and decision records), and `docs/plans/` (implementation plans). There is no build system, no test suite, and no language toolchain configured.

Consequences for working here:

- There are no build/lint/test commands to run. Do not invent them. The toolchain is expected to be Cargo plus a `Makefile` wrapper (see **House Conventions**); update this file with the real commands once the first crate lands.
- `docs/specs/2026-10-01-oxo-architecture-design.md` is **the source of truth for architecture and decisions**. Read it before proposing any design or code. Where a sub-project design contradicts it, that document is wrong and must be amended rather than silently diverged from.
- `README.md` states the problem and the three production phases. It predates the architecture document and is not updated by it; where they differ on execution details, the architecture document governs.

## What This System Is

**Ortho4 XEarthLayer Orchestrator (OXO)** is a control plane that automates production of XEarthLayer regional orthoscenery tiles for X-Plane. It orchestrates `Ortho4XP` rather than reimplementing it.

**v1 scope is phases 1 and 2 only** — specification and production. A run ends when a complete set of ortho tiles and overlays satisfying the specification is in the configured target location and the region's completion gate reports done. Phase 3 (compiling and publishing a regional package with `xearthlayer-publish`) stays with the operator and their existing tooling; OXO does not invoke it.

### Settled execution model

Summarised here because it is the thing most likely to be re-derived incorrectly. The architecture document carries the reasoning and the rejected alternatives.

- The control plane owns an Ortho4XP **pod spec**. "Pod" is the portable unit: Podman and Kubernetes both consume one. There is no abstraction over container runtimes beyond the pod spec, and no second execution driver.
- **Configuration is injected at pod start.** Pods carry none, so configuration drift between workers is not representable.
- **Dispatch is pull.** A pod self-initializes, self-checks capacity, and claims a job only if it has room. Disk pressure therefore throttles the system without any central scheduler.
- **Two job types: ortho and overlay.** A tile's ortho production and its overlay extraction are separate jobs, because they share no data, their resource profiles differ by orders of magnitude, and their dependencies are disjoint. The planner emits up to 2N jobs from N tiles; `include_overlays = false` yields N ortho jobs and is a first-class option, not a degraded mode. There is no overlays-only mode (that would be incremental production, which is out of scope).
- **The platform starts and scales pods, not OXO.** OXO holds no container runtime credentials. It serves work and owns the throughput signal (queue depth, claim/completion/failure rates); scaling automation is out of scope for v1 but the throughput contract is kept open for it.
- **Three volumes:** ephemeral `scratch` (wiped wholesale on cleanup), durable `artifacts` (the deliverable), and a shared persistent `dem-cache` (the one deliberate exception to pod statelessness).
- **Execution mode** decides recycle or stop after cleanup.
- **Job state sits behind a job-server port**, with Postgres as the v1 adapter. The control plane depends on the port, never on Postgres. Pods never reach the persistence layer; they claim and report through the OXO API.

### Assume a homogeneous container platform

Design input is limited to: **a homogeneous platform that can run containers under Podman or Kubernetes.** Do not reason about host counts, per-host core/memory sizing, OS heterogeneity, storage topology, or any existing hand-built Ortho4XP installation — those are deployment concerns, configured in rather than designed around. Workload characteristics (what a single tile costs in disk, memory and time) remain fair design input. Only the local Podman implementation is built for now.

### Why the atomic unit is one 1×1 tile

Ortho4XP's headless entry point is per-tile: `python3 Ortho4XP.py <lat> <lon> [provider_code] [zoomlevel]` runs `build_poly_file → build_mesh → build_masks → build_tile` for a single tile and exits. The job boundary mirrors the tool's own boundary, which is what makes jobs retryable and workers stateless. That is a property of the tool, not a choice.

### Overlays are separate work, and unreachable from the headless CLI

`Ortho4XP.py` calls `build_tile` and stops — it never builds overlays. Extraction is gated on a `do_ovl` *function argument* to `O4_Tile_Utils.build_tile_list` (a batch routine the GUI drives), so a worker must call `O4_Overlay_Utils.build_overlay(lat, lon)` itself.

That function reads only X-Plane's shipped scenery (`custom_overlay_src/Earth nav data/<tile>.dsf`, falling back to `custom_overlay_src_alternate`), a tmp dir and DSFTool. It touches nothing the ortho pipeline produces, and writes to a separate tree — `yOrtho4XP_Overlays/Earth nav data/<10° block>/` versus the ortho tile's `zOrtho4XP_<tile>/`. Hence two independent job types.

**Concurrency hazard:** overlay output is grouped into 10° blocks by `round_latlon`, so every overlay job in one block writes into one shared directory, and Ortho4XP tests for it then creates it (`O4_Overlay_Utils.py:208-209`) — a TOCTOU race that one pod per job makes live. Create that directory idempotently; do not rely on Ortho4XP's check.

### Ortho4XP exits 0 on every failure

Verified 2026-10-01. Every `sys.exit()` in `Ortho4XP.py` is bare, and the build itself is wrapped in a bare `except:` that prints `Crash!` with no exit call at all. Missing directories, unreadable tile config, bad arguments and mid-build exceptions all terminate with status 0; success prints `Bon vol!`. The bare except also discards the traceback, so `Crash!` is the entire diagnostic.

Therefore: **never use exit status to detect Ortho4XP failure.** The intended remedy is a worker entry point that imports the `O4_*` modules and calls the four build functions with real exception handling and real exit codes — no fork of Ortho4XP required.

### Non-goals (hard boundaries)

No bespoke distributed compute platform, job/task management system, or ortho tile processor. Use existing open-source frameworks and `Ortho4XP`. No Kubernetes operator in v1 (compatibility is preserved; the operator is not built).

## Engineering Principles (binding)

From `README.md`, and not optional:

- **TDD, strictly**: every change starts with a failing test specifying the expected behaviour. Red → green → refactor.
- **SOLID**: strict conformance in design and architecture.
- **Gherkin for acceptance criteria**: agreed up front, executed by a cucumber-style framework.
- **Rust for server components**, except where a prerequisite forces otherwise (Ortho4XP is Python).
- **Web/frontend**: HTML5, CSS, JavaScript conforming to modern WCAG principles.
- **End-user documentation** lives in `docs/` and is written only once the API and UX are stable.
- MIT licensed.

## Documentation Conventions

Matching the author's established convention across sibling projects:

- Design documents and decision records: `docs/specs/YYYY-MM-DD-<topic>-design.md`
- Implementation plans: `docs/plans/`
- No separate ADR files — decisions live in a `## Decisions` table inside the relevant design document, alongside `## Open decisions`, `## Out of scope` and `## Rejected alternatives` sections.
- Each sub-project gets its own design document, implementation plan, and implementation cycle. The decomposition into sub-projects is in the architecture document.

## House Conventions (from the sibling `xearthlayer` project)

Follow these when scaffolding; none of it exists in this repository yet.

- A `Makefile` fronts all development tasks, with `make verify` = `format-check + lint + test-strict`, and `make pre-commit` before every push. Docs-only changes are exempt from `pre-commit`.
- Minimum 80% test coverage, target 90%+.
- Traits for abstraction plus dependency injection, so every component is testable in isolation with mocks.

## Related Repositories

Sibling checkouts, not submodules. Read them for interface details rather than guessing — but do not let their deployment topology influence OXO's design.

| Path | Role |
|------|------|
| `/media/Disk6/Projects/Ortho4XP` | Python tile generator the worker pod runs. Entry point `Ortho4XP.py`; per-tile config via `CFG.Tile(lat, lon, …)`; 16 application-level and 44 tile-level config variables in `src/O4_Cfg_Vars.py`; logic in `src/O4_*.py`. |
| `/media/Disk6/Projects/xearthlayer` | Rust workspace for the streaming consumer and the `xearthlayer-publish` binary the operator uses after a run. Its `CLAUDE.md` documents the publisher CLI surface. |
