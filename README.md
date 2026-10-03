# OXO — Ortho4 XEarthLayer Orchestrator

Automated production of [XEarthLayer](https://github.com/samsoir/xearthlayer) regional orthoscenery for X-Plane, by orchestrating [Ortho4XP](https://github.com/oscarpilote/Ortho4XP).

> **Status: 0.1, pre-release.** The specification, control plane and worker pod work end to end (a first production region has been built with them), but interfaces may still change before 1.0.

## What It Does

XEarthLayer streams satellite imagery into X-Plane, but it needs regional scenery packages to know *where* the terrain is. Ortho4XP produces that scenery, one 1×1° tile at a time. Covering a continent means thousands of tiles and weeks of work, with disk pressure, memory pressure, network failures and reboots all able to kill a tile part-way through. Doing that by hand is a full-time job.

OXO turns it into a submit-and-wait operation:

1. You describe a **region** as a list of 1×1° tiles, plus the Ortho4XP settings to use.
2. OXO splits it into **tasks** and serves them to a fleet of worker pods.
3. The workers build the tiles, retry failures, and deliver the results to a target location.
4. OXO tells you when the region is complete.

OXO orchestrates; it does not reimplement. Ortho4XP still builds every tile, and OXO does not compile or publish the final package (that stays with `xearthlayer-publish` and your own tooling).

## How It Works

```
 region spec (TOML)
        │  POST /api/v1/jobs
        ▼
┌─────────────────┐   claims / heartbeats / results (HTTP)   ┌──────────────────┐
│  oxo-controld   │◄─────────────────────────────────────────│  worker pod(s)   │
│  planner + API  │                                          │  oxo-worker      │
│  + reaper       │                                          │   └─ Ortho4XP    │
└────────┬────────┘                                          └────────┬─────────┘
         │ TaskStore port                                             │ deliverable
         ▼                                                            ▼
    PostgreSQL                                                 artifacts volume
```

- **A job is a region; a task is one tile.** The planner turns *N* tiles into up to 2*N* tasks: an *ortho* task and, optionally, an *overlay* task per tile. Submitting the same specification again resumes the existing job rather than starting a new one.
- **Dispatch is pull.** A worker checks it has room (scratch space), claims a task over HTTP, builds it, heartbeats while it works, and reports. Disk pressure throttles the system with no central scheduler, and a reaper reclaims tasks from workers that go quiet.
- **Workers are stateless pods.** Configuration travels with the task and workers hold none, so they cannot drift apart. The same pod spec runs under Podman or Kubernetes, and the platform decides how many to run.
- **Failure is detected honestly.** Ortho4XP exits 0 even when it fails, so the worker drives its build functions directly and reports real success or failure.
- **The deliverable is the XEarthLayer tile**: the DSF, terrain descriptors and mask PNGs (about 64 MB per tile). Textures are generated at runtime by XEarthLayer, so imagery is never kept.

The full reasoning is in the [architecture design](docs/specs/2026-10-01-oxo-architecture-design.md).

## Getting Started

### Requirements

- **Rust** 1.75 or newer ([rustup](https://rustup.rs/))
- **Podman** (or any OCI runtime that can run a pod) for PostgreSQL and the worker image
- For real production: X-Plane's global scenery on disk (Ortho4XP reads it for overlays), enough disk for DEM data, and fast, reliable access to your chosen imagery provider and an Overpass server

### Build and verify

```bash
git clone https://github.com/samsoir/ortho4-xearthlayer-orchestrator.git
cd ortho4-xearthlayer-orchestrator
make verify    # format check, clippy, tests
make help      # every target
```

### Validate a region specification

```bash
cargo run -p oxo-spec-cli -- validate docs/examples/example-region.toml
cargo run -p oxo-spec-cli -- show     docs/examples/example-region.toml
```

[`docs/examples/example-region.toml`](docs/examples/example-region.toml) is a commented starting point. Copy it per region and edit the tiles, metadata, provider, zoom and `target.root`.

### Run the control plane

```bash
podman run -d --name oxo-postgres -e POSTGRES_PASSWORD=<password> \
  -v oxo-pg-data:/var/lib/postgresql/data -p 5432:5432 \
  docker.io/library/postgres:17-alpine

DATABASE_URL='postgres://postgres:<password>@127.0.0.1:5432/postgres' \
  cargo run --release -p oxo-controld -- --bind 0.0.0.0:8080
```

Migrations run at startup. `oxo-controld --help` lists every flag and its environment variable. The API has **no authentication in v1**; run it on a trusted network only.

### Run a worker

```bash
make image                                   # builds oxo-worker:dev
$EDITOR deploy/worker-pod.yaml               # fill in every line marked EDIT ME
podman kube play deploy/worker-pod.yaml
```

The pod spec lists the four volumes a worker needs (DEM cache, X-Plane content, patches, artifacts) and the control plane URL. See [`worker/README.md`](worker/README.md) for the mount-point contract.

### Submit a region and watch it

```bash
curl -s --data-binary @docs/examples/example-region.toml \
  http://127.0.0.1:8080/api/v1/jobs

curl -s http://127.0.0.1:8080/api/v1/jobs/<job_id>              # status
curl -s http://127.0.0.1:8080/api/v1/jobs/<job_id>/throughput   # claim / completion / failure counts
```

When the job reports `complete`, the tiles are in the specification's `target.root`.

## Documentation

| Document | What it covers |
|---|---|
| [Architecture design](docs/specs/2026-10-01-oxo-architecture-design.md) | The source of truth: execution model, decisions, rejected alternatives, sub-project decomposition |
| [High level design](docs/specs/2026-10-01-oxo-high-level-design.md) | The original problem statement and three production phases |
| [Region specification](docs/specs/2026-10-01-region-spec-design.md) | The specification model and its validation rules |
| [Job server](docs/specs/2026-10-01-job-server-design.md) | The task-state port and its PostgreSQL adapter |
| [Control plane](docs/specs/2026-10-02-control-plane-design.md) | The planner, HTTP API and reaper |
| [Worker pod](docs/specs/2026-10-02-worker-pod-design.md) | The worker, the runner and the pod image |
| [Ortho4XP pod contract](docs/specs/2026-10-02-ortho4xp-pod-contract.md) | Measured resource and behaviour facts about Ortho4XP in a pod |
| [Worker image](worker/README.md) | The image's mount-point contract and configuration |
| [Implementation plans](docs/plans/) | How each sub-project was built |

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow and standards (TDD, SOLID, Gherkin acceptance criteria, `make pre-commit`), and the [Code of Conduct](CODE_OF_CONDUCT.md). Report security issues privately as described in [SECURITY.md](SECURITY.md). For questions and live conversation, join us on [Discord](https://discord.gg/RPEWQZdxm2).

## Credits

OXO exists to feed [XEarthLayer](https://github.com/samsoir/xearthlayer), and does its tile production with [Ortho4XP](https://github.com/oscarpilote/Ortho4XP) by Oscar Pilote. The worker image builds the [Shred86 fork of Ortho4XP](https://github.com/Shred86/Ortho4XP), pinned to an exact commit.

Developed with assistance from [Claude](https://claude.ai) by Anthropic.

Made with :heart: in California.

## License

Licensed under the MIT License. See [LICENSE](LICENSE) for details.
