.DEFAULT_GOAL := help
CARGO ?= cargo

.PHONY: help
help: ## Show this help message
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

.PHONY: build
build: ## Build all crates (debug)
	$(CARGO) build --workspace

.PHONY: check
check: ## Fast compile check, no codegen
	$(CARGO) check --workspace --all-targets

.PHONY: test
test: ## Run all tests except those needing a database (see verify-db)
	$(CARGO) test --workspace --exclude oxo-tasks-postgres --all-targets --all-features

.PHONY: test-strict
test-strict: ## Run all tests except database ones, warnings as errors
	RUSTFLAGS="-D warnings" $(CARGO) test --workspace --exclude oxo-tasks-postgres --all-targets --all-features

.PHONY: format
format: ## Format code
	$(CARGO) fmt --all

.PHONY: format-check
format-check: ## Check formatting without modifying
	$(CARGO) fmt --all -- --check

.PHONY: lint
lint: ## Run clippy
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

.PHONY: coverage
coverage: ## Coverage summary (requires cargo-llvm-cov)
	$(CARGO) llvm-cov --workspace --summary-only

.PHONY: verify
verify: format-check lint test-strict ## Format check, lint, test

.PHONY: pre-commit
pre-commit: verify ## REQUIRED before pushing

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean

PG_TEST_CONTAINER ?= oxo-tasks-test-pg
PG_TEST_PORT ?= 55432
PG_TEST_URL ?= postgres://postgres:postgres@127.0.0.1:$(PG_TEST_PORT)/postgres

.PHONY: pg-up
pg-up: ## Start a disposable PostgreSQL for the adapter tests
	podman run --rm -d --name $(PG_TEST_CONTAINER) -e POSTGRES_PASSWORD=postgres -p $(PG_TEST_PORT):5432 docker.io/library/postgres:17-alpine
	printf 'waiting for postgres'
	for i in $$(seq 1 60); do \
	  if podman exec $(PG_TEST_CONTAINER) pg_isready -q -U postgres 2>/dev/null; then echo ' ready'; exit 0; fi; \
	  printf '.'; sleep 1; \
	done; echo ' timed out'; podman rm -f $(PG_TEST_CONTAINER) >/dev/null 2>&1 || true; exit 1

.PHONY: pg-down
pg-down: ## Remove the disposable PostgreSQL
	-podman rm -f $(PG_TEST_CONTAINER) >/dev/null 2>&1 || true

.PHONY: verify-db
verify-db: ## Run the conformance suite against a real PostgreSQL
	$(MAKE) pg-up
	DATABASE_URL=$(PG_TEST_URL) $(CARGO) test --package oxo-tasks-postgres --all-features; \
	status=$$?; $(MAKE) pg-down; exit $$status

.PHONY: image
image: ## Build the worker pod image (podman)
	podman build -t oxo-worker:dev -f worker/Containerfile .

.PHONY: worker-smoke
worker-smoke: image ## Run the runner's fake-O4 success case inside the image
	d=$$(mktemp -d) && cp -r oxo-worker/tests/fixtures/fake_o4 $$d/install && chmod -R a+rwX $$d && \
	printf '{"tile":"+50-002","task_type":"ortho","config":{"v":1,"provider":"BI","zoom":16,"raw":{},"target_root":"/x"},"install_root":"/fake","overlay_src":"/xp"}' > $$d/in; \
	podman run --rm -i --network none -v $$d/install:/fake --entrypoint /opt/oxo/oxo_o4_runner.py oxo-worker:dev < $$d/in > $$d/out; \
	status=$$?; cat $$d/out; \
	if [ $$status -eq 0 ] && tail -n 1 $$d/out | grep -q '"outcome": "ok"'; then rm -rf $$d; echo 'worker-smoke: ok'; \
	else rm -rf $$d; echo "worker-smoke: FAILED (exit $$status)" >&2; exit 1; fi
