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
test: ## Run all tests
	$(CARGO) test --workspace --all-targets --all-features

.PHONY: test-strict
test-strict: ## Run all tests with warnings as errors (matches CI)
	RUSTFLAGS="-D warnings" $(CARGO) test --workspace --all-targets --all-features

.PHONY: format
format: ## Format code
	$(CARGO) fmt --all

.PHONY: format-check
format-check: ## Check formatting without modifying
	$(CARGO) fmt --all -- --check

.PHONY: lint
lint: ## Run clippy
	$(CARGO) clippy --workspace --all-targets -- -D warnings

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
