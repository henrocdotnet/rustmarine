.DEFAULT_GOAL := build

CARGO ?= cargo
ARGS ?=

.PHONY: help build debug install test fmt fmt-check lint check run init doctor scan smoke smoke-managed clean

help: ## List available targets
	@awk 'BEGIN {FS = ":.*## "; print "Rustmarine targets:"} /^[a-zA-Z][a-zA-Z0-9_-]*:.*## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build the release executable (default)
	$(CARGO) build --release --locked

debug: ## Build the debug executable
	$(CARGO) build --locked

install: ## Build and install rustmarine into Cargo's binary directory
	$(CARGO) install --locked --path .

test: ## Run unit and integration tests, excluding opt-in smoke tests
	$(CARGO) test --locked

fmt: ## Format Rust source files
	$(CARGO) fmt

fmt-check: ## Check Rust formatting without modifying files
	$(CARGO) fmt --check

lint: ## Run Clippy with warnings treated as errors
	$(CARGO) clippy --locked --all-targets -- -D warnings

check: fmt-check lint test ## Run formatting checks, Clippy, and tests

run: ## Run the CLI in debug mode; pass arguments with ARGS
	$(CARGO) run --locked -- $(ARGS)

init: ## Provision the managed analysis runtime; accepts ARGS
	$(CARGO) run --release --locked -- init $(ARGS)

doctor: ## Check the analysis runtime; accepts ARGS
	$(CARGO) run --release --locked -- doctor $(ARGS)

scan: ## Scan source changes; accepts ARGS
	$(CARGO) run --release --locked -- scan $(ARGS)

smoke: ## Run the real-engine smoke test using RUSTMARINE_TEST_* settings
	$(CARGO) test --locked --test engine_smoke -- --ignored

smoke-managed: ## Provision and test a managed runtime, including offline reuse and repair
	$(CARGO) test --locked --test managed_smoke -- --ignored

clean: ## Remove Cargo build artifacts
	$(CARGO) clean
