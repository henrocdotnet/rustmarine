.DEFAULT_GOAL := build

CARGO ?= cargo
ARGS ?=
COVERAGE_TOOLCHAIN ?= nightly-2026-10-09
COVERAGE_VERSION := 0.9.1
COVERAGE_DIR ?= target/coverage
COVERAGE_CARGO = $(CARGO) +$(COVERAGE_TOOLCHAIN) llvm-cov

.PHONY: help build debug install test fmt fmt-check lint check run init doctor scan smoke smoke-managed coverage-setup coverage coverage-smoke coverage-report clean

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

coverage-setup: ## Install the pinned nightly compiler, LLVM tools, and coverage reporter
	rustup toolchain install $(COVERAGE_TOOLCHAIN) --profile minimal --component llvm-tools-preview
	$(CARGO) +$(COVERAGE_TOOLCHAIN) install cargo-llvm-cov --locked --version $(COVERAGE_VERSION)

coverage: ## Run regular tests with branch coverage; save HTML, JSON, LCOV, and summary
	$(COVERAGE_CARGO) clean --workspace
	$(COVERAGE_CARGO) --locked --branch --no-report --no-fail-fast
	$(MAKE) coverage-report

coverage-smoke: COVERAGE_DIR = target/coverage-smoke
coverage-smoke: ## Measure regular and real-engine tests together; requires RUSTMARINE_TEST_* settings
	$(COVERAGE_CARGO) clean --workspace
	$(COVERAGE_CARGO) --locked --branch --no-report --no-fail-fast
	$(COVERAGE_CARGO) --locked --branch --no-report --test engine_smoke -- --ignored
	$(MAKE) coverage-report COVERAGE_DIR="$(COVERAGE_DIR)"

coverage-report: ## Export the collected coverage without rerunning tests
	mkdir -p "$(COVERAGE_DIR)"
	$(COVERAGE_CARGO) report > "$(COVERAGE_DIR)/summary.txt"
	cat "$(COVERAGE_DIR)/summary.txt"
	$(COVERAGE_CARGO) report --html --output-dir "$(COVERAGE_DIR)"
	$(COVERAGE_CARGO) report --json --output-path "$(COVERAGE_DIR)/coverage.json"
	$(COVERAGE_CARGO) report --lcov --output-path "$(COVERAGE_DIR)/lcov.info"

clean: ## Remove Cargo build artifacts
	$(CARGO) clean
