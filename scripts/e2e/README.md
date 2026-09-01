# Isolated browser E2E harness

The standard browser test command is cross-platform and owns the complete test
lifecycle:

```sh
pnpm --dir frontend test:e2e
```

It performs these steps in order:

1. Rejects unsafe database names, non-loopback PostgreSQL hosts, insufficient
   `CREATEDB` permission, occupied ports, and non-loopback URL literals in
   `frontend/tests/*.ts`.
2. Drops and recreates one dedicated PostgreSQL database.
3. Starts a loopback-only mock upstream and the Conduit API backend.
4. Waits for `/health`; Playwright then starts Vite and runs the browser suite.
5. Stops all child processes and drops the isolated database, including on
   test failure or interruption.

The default database is `conduit_e2e` on the local PostgreSQL instance used by
`config.example.yml`. Override it with an E2E-specific DSN when necessary:

```sh
CONDUIT_E2E_POSTGRES_DSN='postgresql://conduit:local-test-password@127.0.0.1:5432/conduit_e2e_local' \
  pnpm --dir frontend test:e2e
```

On PowerShell:

```powershell
$env:CONDUIT_E2E_POSTGRES_DSN = 'postgresql://conduit:local-test-password@127.0.0.1:5432/conduit_e2e_local'
pnpm --dir frontend test:e2e
```

The database name must be `conduit_e2e` or start with `conduit_e2e_`, and the
host must be `127.0.0.1`, `localhost`, or `::1`. The harness never reads
`CONDUIT_DB_DSN`, so an ordinary development or production DSN cannot become a
destructive test target by inheritance. The selected E2E database is replaced
on every run; names containing `prod`, `production`, `live`, or `staging` are
also rejected. Use `--keep-db` only for explicit local diagnostics.

## Prerequisites

- Node.js 22 and pnpm 10
- Rust toolchain from `rust-toolchain.toml`
- PostgreSQL client tools (`psql`, `createdb`, and `dropdb`)
- A local PostgreSQL server whose E2E user may create and drop databases
- Playwright Chromium (`pnpm --dir frontend exec playwright install chromium`)
- Cached Rust dependencies (for example,
  `cargo build --locked -p conduit-bin --bin conduit-api`)

Run a non-mutating preflight before starting services:

```sh
pnpm --dir frontend test:e2e:check
```

The preflight validates the target, tools, ports, test URL literals, and
configuration paths. It does not connect to PostgreSQL, create/drop a database,
or start a process.

Playwright arguments pass through unchanged:

```sh
pnpm --dir frontend test:e2e -- --project=setup
pnpm --dir frontend test:e2e:headed
pnpm --dir frontend test:e2e:ui
```

Ports can be changed with `CONDUIT_E2E_BACKEND_PORT`,
`CONDUIT_E2E_FRONTEND_PORT`, and `CONDUIT_E2E_MOCK_PORT`. The backend startup
limit is controlled by `CONDUIT_E2E_BACKEND_TIMEOUT_MS`.

## Network and credential isolation

The harness binds Vite, the backend, and the mock to loopback and accepts only
a loopback PostgreSQL target. It removes inherited `CONDUIT_*` settings and
credential-shaped environment variables from backend and Playwright child
processes, supplies fixed dummy administrator credentials, disables OIDC and
provider quota polling, and sets `CONDUIT_TEST_REAL_PROVIDER=0`.

E2E channel and storage fixtures receive only the local mock URL. The preflight
fails if a spec adds a literal non-loopback HTTP(S) URL. The mock never logs or
echoes request bodies, authorization headers, tokens, or upstream secrets.
Chromium also uses the mock as a fail-closed proxy for non-loopback traffic;
the backend receives the same proxy boundary through standard proxy variables,
and HTTPS tunnelling is rejected. Run the normal Rust/frontend dependency
installation steps before the harness because its child processes cannot fetch
packages from the public network.
