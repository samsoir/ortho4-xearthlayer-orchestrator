# Contributing to OXO

Thanks for your interest in contributing to OXO (Ortho4 XEarthLayer Orchestrator)! This guide covers everything you need to get started.

OXO is a control plane that automates production of [XEarthLayer](https://github.com/samsoir/xearthlayer) regional orthoscenery for X-Plane by orchestrating [Ortho4XP](https://github.com/oscarpilote/Ortho4XP). It orchestrates Ortho4XP rather than reimplementing it; keep that boundary in mind when proposing changes.

## Getting Started

### Prerequisites

- **Rust** (stable, 1.75 or newer) via [rustup](https://rustup.rs/), with `rustfmt` and `clippy`
- **Python 3** on `PATH` — the worker's runner contract tests and the fake-Ortho4XP acceptance run execute `worker/oxo_o4_runner.py`
- **Podman** — for `make verify-db` (a disposable PostgreSQL) and `make image` (the worker pod image)

You do **not** need X-Plane, Ortho4XP or imagery data to run the test suite; the worker's tests use a fake Ortho4XP.

### Setup

```bash
git clone https://github.com/samsoir/ortho4-xearthlayer-orchestrator.git
cd ortho4-xearthlayer-orchestrator
make build    # Debug build
make verify   # Format check + clippy + tests
make help     # Every target, with descriptions
```

### Understand the design first

[`docs/specs/2026-10-01-oxo-architecture-design.md`](docs/specs/2026-10-01-oxo-architecture-design.md) is the source of truth for architecture and decisions. Read it before proposing a design or non-trivial code. Where a sub-project design contradicts it, the architecture document is amended rather than silently diverged from. [`CLAUDE.md`](CLAUDE.md) has a crate-by-crate map of the workspace.

Terminology matters here: a **job** is a region of work; a **task** is one 1×1° tile's conversion.

## Development Workflow

### Branching

OXO uses a single long-lived branch, `main`, which must be releasable at any moment. Work branches off — and its PR targets — `main`.

Branch naming (the prefix names the *change*):

- `feature/<name>` — new functionality
- `bugfix/<issue>-<description>` — bug fixes
- `hotfix/<description>` — urgent fix
- `chore/<description>` — tooling, docs, maintenance

### Before Submitting a PR

```bash
make pre-commit   # fmt + clippy + tests (required)
```

This runs:
1. `cargo fmt --check` — code formatting
2. `cargo clippy -D warnings` — lint with warnings as errors
3. `cargo test` — the full suite (excluding the database adapter) with warnings as errors

Documentation-only changes are exempt.

**`make verify` does not exercise the PostgreSQL adapter.** `oxo-tasks-postgres` is excluded by package name so the gap is visible rather than hidden behind tests that silently skip. If your change touches `oxo-tasks-postgres`, the `TaskStore` port or its conformance suite, also run:

```bash
make verify-db    # starts a disposable PostgreSQL in Podman, runs the conformance suite, tears it down
```

If your change touches the worker image or runner, run `make worker-smoke` too.

PRs that have not passed these checks will not be merged. (There is no CI pipeline yet, so this is on the honour system until there is.)

### Writing Code

OXO follows **SOLID principles** and **Test-Driven Development (TDD)**:

- **Write tests first** — every change starts with a failing test that specifies the expected behaviour: red, green, refactor
- **Use traits for abstraction** — dependency injection over concrete types; task state sits behind the `TaskStore` port, and the control plane never depends on PostgreSQL
- **Keep it testable** — every component should work in isolation with mocks or fakes
- **Gherkin for acceptance criteria** — agreed up front and executed with the cucumber-style scenarios under each crate's `features/` and `tests/acceptance.rs`
- **Target 80%+ test coverage** (90%+ preferred)
- **Rust for server components**, except where a prerequisite forces otherwise (Ortho4XP is Python)
- **Never use Ortho4XP's exit status** to detect failure — it exits 0 on every error. The worker's runner calls the build functions directly for exactly this reason

### Documentation

- Design documents and decision records: `docs/specs/YYYY-MM-DD-<topic>-design.md`
- Implementation plans: `docs/plans/`
- There are no separate ADR files; decisions live in a `## Decisions` table inside the relevant design document, alongside `## Open decisions`, `## Out of scope` and `## Rejected alternatives`

If your change alters behaviour a document describes, update the document in the same PR.

### Commit Messages

Follow the conventional commit format:

```
type(scope): description (#issue)
```

**Types:** `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `perf`

**Examples:**
```
feat(worker): --o4-config-overlay installs site config files over the install root
fix(worker): refuse hollow ortho deliverables — a tile without a DSF never ships
docs(specs): record the patches layout correction
```

### Pull Requests

- Keep PRs focused — one logical change per PR
- Include a clear description of what changed and why
- Reference related issues with `Fixes #N` or `Related: #N`
- Add a test plan section describing how to verify the change
- Respond to review feedback constructively

## What to Work On

- Check [open issues](https://github.com/samsoir/ortho4-xearthlayer-orchestrator/issues) for bugs and feature requests
- Issues labeled `good first issue` are suitable for newcomers
- If you want to work on something, comment on the issue first to avoid duplicate effort
- For anything that changes architecture or a public interface (the region spec, the HTTP API, the pod contract), open an issue to discuss it before writing code

## Reporting Bugs

Use the [bug report template](https://github.com/samsoir/ortho4-xearthlayer-orchestrator/issues/new?template=bug_report.yml). Include:

- The component (`oxo-controld`, `oxo-worker`, `oxo-spec`) and version or commit
- Steps to reproduce, including the region spec if relevant (redact any private paths)
- Expected vs actual behavior
- Relevant log output (`RUST_LOG=info` is usually enough)

## Code of Conduct

All contributors are expected to follow our [Code of Conduct](CODE_OF_CONDUCT.md). Be respectful, constructive, and welcoming.

## Security

Do not report security vulnerabilities in public issues. See the [Security Policy](SECURITY.md).

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
