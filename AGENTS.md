# Repository Guidelines

Conduit API is a standalone Rust and React product. Product identifiers must
use `Conduit API`, `conduit-*`, `conduit_*`, or `CONDUIT_*` as appropriate.
Do not introduce names, paths, URLs, or compatibility shims from predecessor
projects.

## Architecture

- `crates/conduit-bin`: executable and production dependency wiring.
- `crates/conduit-core`: shared domain objects and errors.
- `crates/conduit-admin-graphql`: administrative GraphQL schema.
- `crates/conduit-http`: HTTP routes and protocol handlers.
- `crates/conduit-orchestrator`: candidate selection and request execution.
- `crates/conduit-pipeline`: request and response middleware pipeline.
- `crates/conduit-services`: application services.
- `crates/conduit-db`: PostgreSQL repositories and migrations.
- `frontend`: React administration console.
- `tests/contracts`: committed public API and protocol fixtures.

PostgreSQL is the only supported database. Redis and OpenTelemetry integrations
remain behind explicit Cargo features.

## Validation

Run checks from the workspace root. At minimum, Rust changes require:

```sh
cargo metadata --no-deps --format-version 1
cargo fmt --all -- --check
cargo test -p <affected-package>
cargo clippy -p <affected-package> --all-targets
```

Changes to shared contracts or production wiring require the workspace gates:

```sh
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets
```

Frontend changes require:

```sh
pnpm --dir frontend format:check
pnpm --dir frontend lint
pnpm --dir frontend test:unit
pnpm --dir frontend build
```

Database-backed tests must use an isolated PostgreSQL database:

```sh
CONDUIT_TEST_POSTGRES_DSN=postgresql://conduit:password@127.0.0.1:5432/conduit_test \
  cargo test -p conduit-db
```

Real-provider tests are opt-in and require
`CONDUIT_TEST_REAL_PROVIDER=1` plus provider credentials. Default tests must be
deterministic and credential-free.

## Change Rules

- Preserve unrelated work in a dirty worktree.
- Keep settings connected through UI, GraphQL, PostgreSQL, and runtime behavior.
- Update committed contract fixtures when a public API intentionally changes.
- Never log credentials, tokens, prompt bodies, or raw upstream secrets.
- During the alpha phase, prefer a clean current schema over compatibility code
  for unreleased database layouts.
- Keep documentation factual and remove stale paths when behavior changes.
