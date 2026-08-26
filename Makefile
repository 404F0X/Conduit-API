# Conduit API build and validation targets.
# The Rust server reads the frontend bundle from `frontend/dist/` at runtime.

SHELL := bash

CARGO       ?= cargo
RUSTC       ?= rustc

# Bin crate + binary name (matches crates/conduit-bin/Cargo.toml [[bin]]).
RUST_BIN_CRATE := conduit-bin
RUST_BIN_NAME  := conduit-api

# ---- Phony targets -----------------------------------------------------------

# Build the frontend bundle and release binary.
.PHONY: build build-frontend build-rust-backend
.PHONY: frontend frontend-build frontend/build frontend-test dev
.PHONY: rust-build rust-test rust-lint rust-fmt rust-contract rust-test-fast
.PHONY: rust-test-postgres contract-test migration-test-rust rust-migration-layout
.PHONY: generate-config-schema generate-schema
.PHONY: test
.PHONY: clean-rust check

# ---- S05: default `make build` ----------------------------------------------

build: build-frontend build-rust-backend
	@echo "[build] frontend dist + $(RUST_BIN_NAME) ready."

# Release backend binary. `-p $(RUST_BIN_CRATE)` pins the single binary crate
# (avoids building every test artifact in every lib crate).
build-rust-backend:
	$(CARGO) build --release -p $(RUST_BIN_CRATE)
	@test -x target/release/$(RUST_BIN_NAME) || { \
		echo "build-rust-backend: target/release/$(RUST_BIN_NAME) missing after cargo build" >&2; \
		exit 1; }
	@echo "[build-rust-backend] target/release/$(RUST_BIN_NAME)"

rust-build: build-rust-backend

# ---- Frontend ----------------------------------------------------------------
#
build-frontend frontend frontend-build frontend/build:
	@if [ -d frontend ]; then \
		echo "[build-frontend] pnpm vite build (frontend/dist/ consumed in place by Rust server)…"; \
		cd frontend && pnpm vite build; \
	else \
		echo "[build-frontend] frontend/ absent — skipping (backend-only build)."; \
	fi

frontend-test:
	cd frontend && pnpm test:unit

dev:
	@if [ -d frontend ]; then \
		cd frontend && pnpm vite dev; \
	else \
		echo "dev: frontend/ does not exist." >&2; exit 1; \
	fi

test: rust-test frontend-test

# ---- S03: Rust test / lint / fmt --------------------------------------------

# Workspace-wide test.
rust-test:
	$(CARGO) test --workspace --all-targets

# Same surface, no `--all-targets` (faster feedback for lib-only changes).
rust-test-fast:
	$(CARGO) test --workspace --lib

# Run the PostgreSQL repository/migration suite. Integration cases use
# CONDUIT_TEST_POSTGRES_DSN when it is present.
rust-test-postgres:
	$(CARGO) test -p conduit-db

# Run the same workspace Clippy surface as the release workflow.
rust-lint:
	$(CARGO) clippy --workspace --all-targets

# Format check only (no rewrite). Use `cargo fmt --all` to fix.
rust-fmt:
	$(CARGO) fmt --all -- --check

# ---- S03: config schema -----------------------------------------------------
#
# Builds a temporary schema exporter and validates `config.schema.json`.

generate-schema: generate-config-schema

generate-config-schema:
	@if [ "$(CHECK)" = "1" ]; then \
		bash scripts/generate_config_schema.sh --check; \
	else \
		bash scripts/generate_config_schema.sh; \
	fi

# ---- S03: contract tests ----------------------------------------------------
#
# Verify that the committed public API contract snapshots are present.

contract-test:
	@test -d tests/contracts || { echo "contract-test: tests/contracts/ missing" >&2; exit 1; }
	@test -f tests/contracts/routes_snapshot.md || { echo "contract-test: routes_snapshot.md missing" >&2; exit 1; }
	@test -f tests/contracts/admin_graphql_schema.graphql || { echo "contract-test: admin_graphql_schema.graphql missing" >&2; exit 1; }
	@test -f tests/contracts/openapi_graphql_schema.graphql || { echo "contract-test: openapi_graphql_schema.graphql missing" >&2; exit 1; }
	@echo "[contract-test] committed contract snapshots are present."

rust-contract: contract-test

# ---- S03: migration test (Rust side) ----------------------------------------
#
# CONDUIT_TEST_POSTGRES_DSN must point at an isolated PostgreSQL test database.
migration-test-rust: rust-migration-layout
	@test -n "$(CONDUIT_TEST_POSTGRES_DSN)" || { \
		echo "CONDUIT_TEST_POSTGRES_DSN must point at an isolated PostgreSQL database" >&2; \
		exit 1; }
	$(CARGO) test -p conduit-db

rust-migration-layout:
	bash scripts/db/verify_migrations_layout.sh

# ---- Misc --------------------------------------------------------------------

check:
	$(CARGO) check --workspace

clean-rust:
	$(CARGO) clean
