# OXO architecture design

The system-level architecture for the Ortho4 XEarthLayer Orchestrator
(OXO), and the decision record for the choices every sub-project
inherits. `README.md` states the problem and the three production
phases; this document fixes the execution model, the component
boundaries and the decomposition, and does not restate the README.
v1 covers phases 1 and 2 -- specification and production. Phase 3,
compiling and publishing a regional package, stays with the operator
and their existing tooling.

It is the source of truth for architecture and decisions. Sub-project
designs under `docs/specs/` refine it; where one contradicts this
document, this document is wrong and must be amended rather than
silently diverged from.

## Goal

A control plane that takes a validated region specification and drives
it to a complete set of produced 1x1 degree ortho tiles and overlays
in a configured target location, without manual orchestration,
surviving the failure modes that make the manual
process expensive: memory and disk exhaustion, network failure, host
restarts and operating system updates.

Success:

- A region specification is submitted once and reaches completion
  without an operator dispatching individual tiles.
- The output is a complete set of ortho tiles and overlays satisfying
  the specification, in the configured target location, ready for the
  operator's existing packaging tools.
- A tile that fails is retried under a declared policy, and a tile
  that exhausts its retries is reported rather than silently dropped.
- The control plane restarting loses no task state.
- A worker pod that dies mid-tile has its work reclaimed and reissued.
- No worker carries persistent configuration, so configuration drift
  between workers is not representable.

## Two nouns: job and task

Fixed here because every sub-project uses both, and the words were
previously used the other way round.

- A **job** is a region of work to compile: one submission of one region
  specification revision. It is what an operator asks for.
- A **task** is one unit that converts a single 1x1 degree tile into
  something useful. A job consists of up to 2N tasks for N tiles —
  an ortho task per tile, and an overlay task per tile when the
  specification asks for them.

Where this document says "the job server", it means the component that
holds jobs and dispatches their tasks; its persistence port is the task
store.

## Platform assumption

**A homogeneous platform that can run containers under Podman or
Kubernetes.** That is the only runtime constraint this design reasons
about. It holds for a local fleet and for cloud alike, which is why it
is the constraint chosen.

Deliberately excluded from design input: host counts, per-host core
and memory sizing, operating-system heterogeneity, storage topology
and any existing hand-built Ortho4XP installation. Those are
deployment concerns, configured into the system rather than designed
around. Workload characteristics -- the disk and memory a single tile
consumes, the external services a build depends on -- remain fair
design input, because they constrain the system wherever it runs.

Only the local Podman implementation is built for now. Kubernetes
compatibility is preserved by construction, not by a second code path:
the unit of execution is a pod spec, which both runtimes consume.

## Architecture

```
  region spec ──────▶┌─────────────────────────────────┐
  (validated)        │  Control plane                  │
                     │   planner · claim API           │
                     │   pod-spec authority            │
                     │   config injection · throughput │
                     └───────────┬─────────────────────┘
                                 │  job-server port
                     ┌───────────▼─────────────────────┐
                     │  Job server                     │
                     │   tasks · leases · retries       │
                     │   [Postgres adapter, v1]        │
                     └─────────────────────────────────┘
                                 ▲
                                 │  claim / heartbeat / complete / fail
                                 │  (OXO API — pods never see the store)
         ┌───────────────────────┴───────────────────────┐
         │                                               │
   ┌─────▼──────┐                                 ┌──────▼─────┐
   │ worker pod │     N pods, started and scaled  │ worker pod │
   │  Ortho4XP  │     by the platform, not by OXO │  Ortho4XP  │
   └─────┬──────┘                                 └────────────┘
         │
         ├── scratch    ephemeral, wiped on cleanup
         ├── artifacts  durable, egress target — the v1 deliverable
         └── dem-cache  persistent, shared between pods
```

### The pod is the unit of execution

The control plane owns an Ortho4XP **pod spec**. "Pod" is the portable
noun: Podman and Kubernetes both consume one, so the local and cloud
stories are the same design with different backends. There is no
abstraction over container runtimes beyond the pod spec itself, and no
second execution driver.

### Two task types: ortho and overlay

A tile's ortho production and its overlay extraction are **separate
tasks**. The planner emits up to two tasks per tile, and a region is
complete when every task of both types has succeeded.

This follows from Ortho4XP rather than from preference.
`O4_Overlay_Utils.build_overlay(lat, lon)` reads only X-Plane's
shipped scenery -- `custom_overlay_src/Earth nav data/<tile>.dsf`,
falling back to `custom_overlay_src_alternate` -- plus a temporary
directory and DSFTool. It touches nothing the ortho pipeline produces:
no mesh, no imagery, no `Tiles/`, no `Orthophotos/`. It writes to a
separate tree, `yOrtho4XP_Overlays/Earth nav data/<10 degree block>/`,
where ortho output goes to `zOrtho4XP_<tile>/`. The two have no data
dependency in either direction.

Keeping them apart buys four things:

- **Resource profiles that are not comparable.** An overlay task is a
  file copy, a DSFTool conversion and some text processing: minutes
  and megabytes. An ortho task is imagery download plus mesh
  generation: hours, and hundreds of gigabytes on observed figures.
  Bundled, the light work would have to reserve the heavy work's
  footprint, and admission control could not pack many overlay tasks
  into the space one ortho task needs.
- **Failure isolation.** Bundled, an overlay fault at the end of a
  tile would retry hours of completed ortho work.
- **Disjoint dependencies.** Ortho needs the imagery provider and
  Overpass; overlay needs the X-Plane overlay source and DSFTool. A
  provider outage stalls ortho tasks while overlay tasks keep draining,
  and the reverse holds. Bundled, either outage blocks everything.
- **No egress contention**, since the output trees are separate.

`include_overlays` in the region specification therefore decides
whether the planner emits overlay tasks at all, rather than changing
what a worker does inside a single task. A region with
`include_overlays = false` produces N tasks, all ortho; one with it set
produces 2N. Building a region without overlays is a first-class
option, not a degraded mode.

There is deliberately no overlays-only mode. Adding overlays to a
region whose ortho tiles already exist is incremental production,
which is out of scope for v1, so the flag is a boolean rather than a
choice of three.

### Pod lifecycle

1. The platform starts a pod from the spec the control plane owns.
2. The control plane injects configuration at start. The pod ships
   with no configuration of its own.
3. The pod **self-initializes** from that configuration -- it stages
   what it needs rather than being pre-staged by the orchestrator.
4. The pod **self-checks capacity** and claims a task only if it has
   room to complete one. An overlay task's footprint is a small
   fraction of an ortho task's, so the same pod may accept one and
   decline the other.
5. The pod does the task's work -- ortho production, or overlay
   extraction, for one tile.
6. The pod egresses artifacts to the durable volume.
7. The pod **cleans up as defined** -- scratch is wiped wholesale.
8. The pod **recycles or stops**, per its execution mode.

Steps 4 through 8 are the loop in recycle mode; in one-shot mode the
pod exits after its first cleanup.

### Dispatch is pull, not push

A pod is started generically and then claims work. Injected config
carries *how* to work and where the control plane is; the claim
carries *what* tile to work on.

This is what makes recycle mode meaningful -- a recycling pod needs a
way to fetch its next tile -- and it puts capacity assessment in the
only place that can measure it honestly, which is the pod itself. A
pod that cannot fit a tile does not claim one, so disk pressure
throttles the system without any central scheduler. One-shot mode is
the same protocol with a claim limit of one.

### Pod lifecycle is the platform's task

OXO does not create, scale or reap pods, and holds no credentials for
a container runtime API. Worker pods are started by the platform:
systemd or Quadlet units locally, a Deployment or operator under
Kubernetes. OXO serves work.

This keeps OXO out of the scheduling business that the README's
non-goals warn against, and it means local and cloud deployments
differ in their unit files, not in OXO.

OXO does own the **throughput signal** -- queue depth, claim rate,
completion rate, failure rate -- and must be able to express a desired
worker count. Nothing acts on it in v1. It is a boundary, kept open so
that scaling automation is additive later: such automation reads
throughput from OXO and either actuates the platform itself or
publishes a desired state for the platform to converge on.

### Volumes

| Volume | Lifetime | Contents |
|---|---|---|
| `scratch` | Ephemeral, per pod | All intermediate work. Wiped wholesale on cleanup. |
| `artifacts` | Durable | Finished ortho tiles (`zOrtho4XP_<tile>/`) and overlays (`yOrtho4XP_Overlays/`). The egress target and the v1 deliverable: the operator points existing packaging tools at it. |
| `dem-cache` | Persistent, shared | Elevation data, which covers more area than one 1x1 degree tile and is therefore worth retaining across tasks and pods. |
| `content` | Read-only, shared | **Added 2026-10-02 by the worker pod design.** Source material the build consumes but never produces: the X-Plane Global Scenery / demo data that overlay extraction reads (`custom_overlay_src`), and the patches tree that region specifications select from by name. Exists because the image carries tools, never data — this content is large and deployment-specific, so it reaches the pod as a mount whose backing store (local disk, NAS, bucket driver) OXO never sees. |

Working on scratch rather than directly on the durable volume keeps
Ortho4XP's heavy intermediate I/O local, makes cleanup trivially
correct, and means a crashed pod cannot leave partial state in the
tree the operator's packaging tools read.

The DEM cache is the one deliberate exception to "nothing survives a
pod". It buys real time across a region, and it costs OXO a cache
lifecycle: concurrent access must be safe for multiple pods, and its
disk use must be accounted for rather than unbounded.

## What Ortho4XP's headless path forces on us

Verified against the sibling `Ortho4XP` checkout on 2026-10-01.
Recorded because it is not inferable from the README and it changes
the worker design.

The headless entry point is
`python3 Ortho4XP.py <lat> <lon> [provider_code] [zoomlevel]`, which
runs `build_poly_file` -> `build_mesh` -> `build_masks` -> `build_tile`
for one tile and exits. The 1x1 degree task boundary therefore mirrors
the tool's own boundary, which is what makes a task retryable and a
worker stateless. That is a property of the tool, not a choice.

**The headless path never builds overlays.** `Ortho4XP.py` calls
`build_tile` and stops. Overlay extraction is gated on a `do_ovl`
function argument to `O4_Tile_Utils.build_tile_list`, a batch routine
the GUI drives, so a worker shelling out to `Ortho4XP.py` cannot
produce overlays at all and must call `O4_Overlay_Utils.build_overlay`
itself. This is part of why ortho and overlay are separate task types.

**Exit status is not a failure signal.** Every `sys.exit()` in
`Ortho4XP.py` is bare, so it exits 0. The build itself is wrapped in a
bare `except:` that prints `Crash!` and falls off the end of the script
with no exit call at all -- also 0. Missing install directories,
unreadable tile configuration, bad arguments and a mid-build exception
all terminate successfully as far as the operating system is
concerned. Success prints `Bon vol!`.

**Failure diagnostics are a single word.** The bare `except:` discards
the traceback, so `Crash!` is the entire diagnostic. Nothing in the
process output distinguishes a transient network failure from disk
exhaustion from a genuine data error -- which is precisely the
distinction a retry policy needs.

Consequences, which sub-projects 0 and 4 must resolve rather than
rediscover:

- A worker determines success from produced artifacts and output
  markers, never from exit status.
- Coarse failure classification is the default, and a retry policy
  built on it will retry things that cannot succeed.
- The preferred remedy is for the worker to use its own entry point
  that imports the `O4_*` modules and calls the build functions it
  needs -- the four ortho steps, or `build_overlay` -- with real
  exception handling and real exit codes, rather than shelling out to
  `Ortho4XP.py`. This needs no fork of Ortho4XP, is the cheapest route
  to a usable failure taxonomy, and is the only way to reach overlay
  extraction at all.
- Overlay output is grouped into 10 degree blocks by `round_latlon`,
  so every overlay task in one block writes into a single shared
  destination directory -- and Ortho4XP tests for that directory and
  then creates it (`O4_Overlay_Utils.py:208-209`), a time-of-check to
  time-of-use race that one pod per task makes live. The worker must
  create that directory idempotently rather than relying on
  Ortho4XP's check.

## Component boundaries and ports

Strict conformance to SOLID is a project principle; these are the
boundaries that principle produces here.

| Component | Responsibility | Depends on |
|---|---|---|
| Region spec | Model and validation of a region: metadata, its explicit set of 1x1 degree tiles, and production parameters | Nothing |
| Job server | Durable task state, lease/claim/heartbeat/expiry, retry accounting, region-completion gate | A persistence adapter |
| Control plane | Atomize a spec into tasks, serve the claim API, own the pod spec, inject config, expose throughput | Region spec, job-server port |
| Worker pod | Self-init, capacity check, tile production, egress, cleanup, recycle/stop | Injected config, OXO API |

**The job-server role is separated from the control plane by an
explicit port.** Queue persistence is a trait with lease, claim, retry
and completion semantics; Postgres is the v1 adapter behind it. The
control plane depends on the port, never on Postgres. Postgres is a
hard dependency of the v1 deployment, not of the design.

**Pods never reach the persistence layer.** They claim and report
through the OXO API. This keeps the pod thin, keeps store credentials
out of workers, keeps the adapter swappable, and gives acceptance
tests a far better surface than a broker protocol.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Runtime constraint | Containers under Podman or Kubernetes, homogeneous | Holds for local and cloud alike; the only constraint worth designing against |
| Unit of execution | A pod spec owned by the control plane | Portable across both runtimes without a second code path |
| Configuration ownership | Injected at pod start; pods carry none | Configuration drift between workers becomes unrepresentable rather than merely discouraged |
| Region definition | The specification takes an explicit set of 1x1 degree tiles | How a human composes that set is a UX concern, specified separately with the web interface; the model must not be shaped by its authoring tool |
| v1 deliverable | A complete ortho and overlay tile set in a target location | Packaging and publishing stay with the operator's existing tooling; even this much is a large improvement on the manual process |
| Task types | Ortho and overlay are separate tasks | They share no data, their resource profiles differ by orders of magnitude, and their dependencies are disjoint; bundling would make light work reserve heavy work's footprint and let either dependency's outage block everything |
| Dispatch | Pull — pods claim tiles | Makes recycle mode coherent; puts capacity assessment where it can be measured |
| Pod lifecycle management | The platform's, not OXO's | Conforms to the "no bespoke compute platform" non-goal; local and cloud differ in unit files only |
| Scaling automation | Out of scope for v1; throughput signal kept as a contract | Additive later; building it now is unjustified |
| Work location | Ephemeral scratch, egress to durable volume | Local intermediate I/O; wholesale cleanup; no partial state in the delivered tree |
| DEM cache | Persistent and shared across pods | Elevation data spans more than one tile, so re-downloading per task is pure waste |
| Execution mode | Recycle or stop, per configuration | The README's container-lifetime tension is a parameter, not a design choice |
| Job substrate | Postgres behind a job-server port: a domain `tasks` table claimed with `SELECT … FOR UPDATE SKIP LOCKED`, leases held by heartbeat rather than a fixed duration | Specs, the completion gate and throughput are all queries; one store beats a store plus a broker. Settled in the job server design after a survey found no mature Rust Postgres queue crate, and none able to answer the domain questions this system asks |
| Failure detection | Artifacts and output markers | Ortho4XP's headless path exits 0 on every failure |
| Spec convention | `docs/specs/YYYY-MM-DD-<topic>-design.md`, plans in `docs/plans/` | Matches the author's established convention across sibling projects |
| End user documentation | Lives in `docs/`, completed once the function is completed to provide end users guidance on functionality. | End user docs must be written only when the api and ux are stable. |

## Decomposition into sub-projects

The system is too large for one specification. Each sub-project below
gets its own design document, implementation plan and
implementation cycle.

| # | Sub-project | Scope |
|---|---|---|
| 0 | *(spike)* Ortho4XP pod contract | One tile built headless in a container. Measured peak scratch, memory and wall-clock. Exit and failure taxonomy. What configuration must be injected. What the DEM cache actually saves. Output is numbers and an answer; anything built is throwaway. |
| 1 | Region spec: model and validation | Specification data model over an explicit set of 1x1 degree tiles, plus metadata, production parameters and validation rules, with a CLI. Pure library: no persistence, no runtime, no network. |
| 2 | Job server: port and Postgres adapter | Task lifecycle state machine, lease/heartbeat/expiry, retry policy and accounting, region-completion gate across both task types, Postgres adapter behind the port. |
| 3 | Control plane: planner and claim API | Atomize a specification into per-tile ortho and overlay tasks — up to 2N tasks from N tiles, overlay tasks only when the specification asks for them — serve claim/heartbeat/complete/fail, drive lease expiry, expose the throughput signal. **Amended 2026-10-02:** configuration injection and pod-spec ownership moved to sub-project 4 — spike 0 had not run and there was no worker image for a pod spec to describe, so designing either here would have been invention. See the control plane design document. |
| 4 | Ortho4XP worker pod | Image, self-initialization, capacity check, ortho production and overlay extraction, artifact egress, cleanup, recycle and stop modes. Must create the shared overlay destination directory idempotently. **Amended 2026-10-02:** also owns the pod spec and the configuration-injection surface, moved from sub-project 3, so the image, the injection surface and the pod spec are designed against each other. |
| 5 | Observability and operator interface | Telemetry export, failure policy and alerting, operator views in HTML5/CSS/JS to WCAG principles. |

Sequence: this document, then sub-project 1. Spike 0 is deferred
rather than dropped -- its numbers are needed before the resource
model in sub-projects 2 through 4 can be more than a guess.

Compilation is not a sub-project. A run ends when the artifacts are
in place and the region's completion gate reports done; packaging and
publishing are the operator's, using the tooling they already have.

One further document is known to be needed and is not a sub-project
here: a specification for **tile-set authoring UX**, covering broad
selection methods such as bounding areas. It belongs with the web
interface.

## Open decisions

- **Per-tile resource estimation.** Admission control needs an
  expected footprint per tile, presumably a function of zoom level,
  provider and whether overlays are included. Spike 0 supplies the
  numbers. **Amended:** this was assigned to sub-project 2, which cannot
  carry it — the numbers do not exist until spike 0 runs, and the
  consumer is the worker's capacity self-check rather than the task store.
  Until then a worker expresses capacity by filtering the task types it
  will claim, which is sufficient because an overlay task's footprint is a
  small fraction of an ortho task's. When the numbers exist the estimate
  becomes a column on the task record and a predicate in the claim query,
  which is additive.
- **DEM cache concurrency and accounting.** Safe shared access for
  concurrent pods, and a bound on its growth.
- **Retry classification.** Whether a usable transient-versus-permanent
  distinction is achievable via the custom entry point, or whether the
  policy must assume every failure is retryable up to a limit.
- **Where execution mode is set.** Whether recycle-or-stop is a
  property of the pod spec and its deployment, or of the region
  specification that the work belongs to. The former makes it an
  operational tuning knob; the latter makes it part of the
  reproducible definition of a package. **Amended 2026-10-02: settled
  by the worker pod design** — it is a pod-level operational knob (an
  environment variable in the pod spec). With region intent travelling
  on the task itself, nothing regional remains in the mode: `recycle`
  suits a long-lived Podman pod, `stop` suits a Kubernetes Job, and the
  region's reproducible definition is untouched by either.
- **Where the Ortho4XP build is pinned.** Workers must agree on a
  version, and version skew is a recorded failure mode of the manual
  process. Whether OXO asserts provenance or merely records it is open.

## Out of scope

- **Packaging and publishing.** v1 delivers tiles and overlays to a
  target location. Creating and releasing regional scenery packages
  with `xearthlayer-publish` stays with the operator and the tooling
  they already have. OXO does not invoke it.
- **Incremental production.** v1 produces the complete set its
  specification names. Targeting only gaps or additions against an
  already-published region is deferred sophistication, not a v1
  requirement.
- **Tile-set authoring UX.** The specification consumes an explicit
  set of tiles. Broad selection methods -- bounding areas and similar
  -- belong to the web interface and get their own specification.
  Composing large sets in Ortho4XP today takes thousands of mouse
  clicks and is error-prone, which is the problem that document will
  address; it is not a reason to complicate the specification model.
- Scaling automation and any OXO-held container runtime credentials.
- A Kubernetes operator. Compatibility is preserved; the operator is
  not built.
- Any bespoke distributed compute platform, job or task management system, or
  ortho tile processor, per the README's non-goals.

## Rejected alternatives

**An execution-slot abstraction with multiple drivers**, one per
runtime including native host processes. Rejected: the homogeneous
container platform assumption removes the need, and a pod spec is
already the portable unit. It would have bought heterogeneity nobody
asked for at the cost of the only abstraction that matters being
duplicated.

**Push dispatch, one pod per tile.** The control plane would create a
pod per task with the tile baked into its configuration -- simplest
possible pod, no claim protocol. Rejected: recycle mode loses its
meaning, every tile pays cold start and cache rebuild, and it forces
OXO to hold runtime credentials and make placement decisions.

**OXO driving the container runtime API.** Central admission control
and one place to reason about fleet state. Rejected: by far the
largest surface, two runtime integrations to build and test, and it
makes OXO the scheduler the non-goals warn against. Capacity
self-assessment in a pulling pod achieves the same throttling.

**NATS JetStream as the substrate.** Ack-timeout leases and
max-deliver retry limits natively, in one light clusterable binary.
Rejected for v1: a queue is not a database, so task state and the
completion gate need a store alongside it -- two systems where one
suffices. As built, the job-server port requires stored-task-set
comparison for `create_job` idempotency, per-job state aggregates for
`job_status` and `throughput`, and attempt accounting -- none of which
a message queue provides without that companion store, the pairing
this rejection is about. The port is therefore an honest *database*
port, not a substrate port; a future adapter is anything that answers
those queries, such as SQLite, not a queue.

**Temporal.** The strongest conformance to "no bespoke task system",
with durability, retries and heartbeats as the product. Rejected:
substantial operational weight, and a poor fit for pull-based Python
worker pods, which are not Temporal workers and would need a shim.

**Working directly on the durable volume.** No egress step, and
nothing can be lost between production and packaging. Rejected:
Ortho4XP's intermediate I/O would cross the network, cleanup would
have to be surgical rather than wholesale, and a crashed pod would
leave partial state where the operator's tooling reads.

**Scratch and egress with no persistent cache.** Cleaner accounting
and a genuinely stateless pod. Rejected: elevation data covers more
than one tile, so this re-downloads it for every task in a region.

## Dependencies

- **Ortho4XP** -- tile production. Headless per-tile entry point;
  reads a configuration surface of 16 application-level and 44
  tile-level variables (`src/O4_Cfg_Vars.py`, counted 2026-10-01), of
  which the tile-level set is what a per-task configuration must
  supply; needs its overlay source as a real directory.
- **A reachable Overpass endpoint** for vector data, named by the
  injected configuration. Public servers rate-limit and truncate
  under load, which is a recorded cause of build failure; the
  configuration must be able to name several.
- **DSFTool and an X-Plane overlay source tree**, for overlay tasks
  only. `build_overlay` copies the shipped `.dsf` for the tile and
  converts it DSF -> text -> DSF, so an overlay task needs neither the
  imagery provider nor Overpass.
- **`xearthlayer-publish`** -- not invoked by OXO, and not a runtime
  dependency. It is the operator's tool for the packaging step that
  follows a run, and it reads Ortho4XP output from a POSIX path --
  which is what fixes the artifacts volume as a filesystem rather
  than an object store.
- **PostgreSQL** -- the v1 job-server adapter. A deployment
  dependency, not a design one.

## Related

- `README.md` -- problem statement, the three production phases,
  non-goals and engineering principles.
- `CLAUDE.md` -- orientation for future sessions.
