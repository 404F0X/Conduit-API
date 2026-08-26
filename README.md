# Conduit API

[![Release gates](https://github.com/404F0X/Conduit-API/actions/workflows/release-gates.yml/badge.svg)](https://github.com/404F0X/Conduit-API/actions/workflows/release-gates.yml)
[![CodeQL](https://github.com/404F0X/Conduit-API/actions/workflows/codeql.yml/badge.svg)](https://github.com/404F0X/Conduit-API/actions/workflows/codeql.yml)

Conduit API is a self-hosted AI gateway for managing upstream providers,
model routing, prompt-cache affinity, usage accounting, and customer billing.
The project is under active development and its first public releases are
published as alpha software.

## Capabilities

- OpenAI-compatible, Anthropic, Gemini, Jina, Doubao, and AI SDK protocol
  translation.
- Health-aware multi-provider routing, retries, limits, and cache affinity.
- Provider model discovery, automatic mapping rules, pricing observations, and
  reviewable change drafts.
- Accounting-currency procurement costs with customer-facing credit pricing.
- PostgreSQL-backed projects, API keys, usage records, wallets, subscriptions,
  audit history, and backup/restore.
- React administration console and GraphQL automation APIs.

## Quick Start

Docker Compose is the supported evaluation path. It starts Conduit API and a
persistent PostgreSQL 17 database, binds the application to loopback, and does
not provide a default application administrator password.

```sh
export CONDUIT_POSTGRES_PASSWORD='replace-with-a-long-random-value'
docker compose config --quiet
docker compose up --build -d
curl -fsS http://127.0.0.1:8090/health
```

Open `http://127.0.0.1:8090` and create the first owner account. See
[Production Deployment](docs/production-deployment.md) before exposing an
instance outside the local machine.

## Releases

A tag that exactly matches the workspace version, such as
`v0.1.0-alpha.1`, runs the complete release gates before publishing signed
`linux/amd64` and `linux/arm64` images to
`ghcr.io/404f0x/conduit-api`. Each release records the immutable image digest;
the registry image carries an SBOM and build provenance attestation.

## Build From Source

Requirements:

- Rust 1.96.0, pinned by `rust-toolchain.toml`
- Node.js 22
- pnpm 10.23.0
- PostgreSQL 17

```sh
corepack enable
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend build
cargo build --release -p conduit-bin --bin conduit-api

CONDUIT_DB_DSN='postgresql://conduit:password@127.0.0.1:5432/conduit' \
  ./target/release/conduit-api --config config.example.yml
```

The source configuration binds HTTP and metrics to `127.0.0.1` by default.
Use deployment-specific environment variables and a TLS reverse proxy for
networked installations.

## Development

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets
pnpm --dir frontend test:unit
pnpm --dir frontend lint
pnpm --dir frontend build
```

PostgreSQL-backed integration tests require an isolated database:

```sh
CONDUIT_TEST_POSTGRES_DSN='postgresql://conduit:password@127.0.0.1:5432/conduit_test' \
  cargo test -p conduit-db
```

The compatibility and release checks are documented in
[Release Gates](RELEASE_GATES.md).

Current priorities are tracked in the [Roadmap](ROADMAP.md). Conduit API is
pre-release software; evaluate it with a fresh database and review the
deployment guide before production use.

## Repository

- Source: <https://github.com/404F0X/Conduit-API>
- Backend: `crates/*`
- Web console: `frontend/`
- PostgreSQL migrations: `migrations/postgres/`

## License

Most of the repository is Apache-2.0. The protocol core crates listed in
[LICENSE](LICENSE) remain LGPL-3.0-only. Required attributions are in
[NOTICE](NOTICE) and [frontend/NOTICE](frontend/NOTICE).
