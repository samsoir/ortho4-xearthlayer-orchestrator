# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository Status

**This repository contains no code yet.** It holds `README.md` (the design document) and an empty `docs/`. There are no commits on `main`, no build system, no test suite, and no language toolchain configured.

Consequences for working here:

- There are no build/lint/test commands to run. Do not invent them. When the first implementation lands, the toolchain is expected to be Cargo + a `Makefile` wrapper (see **House Conventions**), and this file should be updated with the real commands at that point.
- The README is the authoritative specification. Read it before proposing any design or code — it defines the problem, the three-phase architecture, explicit non-goals, and binding engineering principles.
- Treat `docs/` as the home for design documents that elaborate on the README.

## What This System Is

**Ortho4 XEarthLayer Orchestrator (OXO)** is a control plane that automates production of XEarthLayer regional orthoscenery packages for X-Plane. It orchestrates two existing tools rather than reimplementing their work: `Ortho4XP` (tile generation) and `xearthlayer-publish` (package compilation).

The pipeline has three phases, and the phase boundaries are the primary architectural seams:

1. **Specification** — a regional scenery package is declared as an explicit enumeration of 1×1 degree tiles (a region ≈ a continent, e.g. `NA`, `OC`), plus Ortho4XP parameters, filesystem locations for X-Plane global/demo scenery input and ortho/overlay output, and retry/alerting policy. Must be validated before it enters production.
2. **Production** — the specification is atomized into _N_ jobs, **one job per 1×1 degree tile** (ortho + optional overlays), each completed single-shot in isolation by a worker. The orchestrator is responsible for validating config, staging/reachability-checking resources and filesystem mounts, and exporting telemetry.
3. **Compilation** — only after *every* job for a region succeeds, a single node with access to the XEarthLayer package library runs `xearthlayer-publish` to produce the final package. Publishing is explicitly deferred out of early versions.

### Why the atomic unit is one 1×1 tile

This is not an arbitrary choice: Ortho4XP's headless entry point is per-tile. `python3 Ortho4XP.py <lat> <lon> [provider_code] [zoomlevel]` runs the full `build_poly_file → build_mesh → build_masks → build_tile` sequence for a single tile and exits. The job boundary mirrors the tool's natural boundary, which is what makes jobs retryable and workers stateless.

### Execution-model constraint

The design must satisfy two runtimes at once:

- **Near term / concrete target**: the author's home network — 3–4 non-clustered nodes, each running 4–8 concurrent containers. This requires a **per-node controller** that coordinates work on that node and connects back to the control plane; containers themselves know only how to execute a job handed to them.
- **Longer term**: a first-class Kubernetes operator. K8s is *not* a near-term requirement, but abstractions must stay compatible with it.

The tension to keep in mind: container lifetime is expected to be one job, but K8s can scale a spec itself whereas Podman is an atomic runtime needing an external controller for process lifecycle. Do not design something that only works for one of the two.

### Non-goals (from the README — these are hard boundaries)

Do not build a bespoke distributed compute platform, a bespoke job/task management system, or a bespoke ortho tile processor. Use existing open-source frameworks and `Ortho4XP`. Publishing functions are out of scope for early versions.

## Engineering Principles (binding)

These come from the README and are not optional:

- **TDD, strictly**: every piece of work starts with a failing test that specifies the expected behaviour, then implementation against that test. Red → green → refactor.
- **SOLID**: strict conformance in design and architecture.
- **Gherkin for acceptance criteria**: acceptance criteria are agreed up front and expressed in Gherkin, run by a cucumber-style framework. Requirements are shared in that DSL.
- **Rust for server components**, except where a prerequisite dependency forces otherwise (Ortho4XP is Python; many distributed compute frameworks are Go).
- **Web/frontend**: HTML5, CSS, JavaScript, well structured, conforming to modern WCAG principles.
- MIT licensed.

## House Conventions (from the sibling `xearthlayer` project)

This project is part of a family of repos under `/media/Disk6/Projects/`. The established conventions there are the ones to follow when scaffolding this one:

- A `Makefile` fronts all development tasks, with `make verify` = `format-check + lint + test-strict`, and `make pre-commit` run before every push. Docs-only changes are exempt from `pre-commit`.
- Minimum 80% test coverage, target 90%+.
- Traits for abstraction plus dependency injection, so every component is testable in isolation with mocks.

## Related Repositories

These are sibling checkouts, not submodules — read them for interface details rather than guessing:

| Path | Role |
|------|------|
| `/media/Disk6/Projects/Ortho4XP` | Python tile generator invoked by production jobs. Entry point `Ortho4XP.py`; per-tile config via `CFG.Tile(lat, lon, ...)`; logic in `src/O4_*.py`. |
| `/media/Disk6/Projects/xearthlayer` | Rust workspace for the streaming consumer and the `xearthlayer-publish` binary used in the compilation phase. Its `CLAUDE.md` documents the publisher CLI surface (`scan`, `add`, `build`, `urls`, `version`, `release`, `validate`). |
