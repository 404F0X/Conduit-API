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
3. Creates an owned temporary journal/log directory and starts a loopback-only mock upstream and the Conduit API backend.
4. Waits for `/health`; Playwright then starts Vite and runs the browser suite.
5. Stops all child processes, removes only the owned temporary runtime directory, and drops the isolated database, including on
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

## Playground billing regression

`frontend/tests/playground-billing.spec.ts` is collected by the existing Chromium
project and the release-gates job's `pnpm test:e2e -- --reporter=line` command.
No separate CI invocation or skipped/missing-DSN success path is used. For focused
local diagnostics only, pass `playground-billing.spec.ts --reporter=line`.

The harness explicitly starts the isolated fixture backend in Hard billing mode;
this does not change the product's default mode. Its local OpenAI mock supports
both buffered responses and deterministic SSE (including the final usage event
and `[DONE]`). It never echoes prompts or authentication data.

Each financial fixture creates a dedicated test Project wallet and a normal user API key created
and revealed through the browser, a published retail price of 2/4 CNY per million
input/output tokens, 10000 station credits per CNY, and Project multiplier 1.5.
The stream reports 100 input and 20 output tokens: exactly 4.2 credits (4200000
microcredits). The browser checks market prices, sends a real Playground request,
then opens request activity and the wallet. Bounded SQL polling checks this key's
one usage, charge, settlement and ledger debit, exact remaining balance, and zero
active reservation. Only test prerequisites are seeded through SQL in the guarded
isolated database; inference, authentication and settlement use the real backend.
Screenshots, traces and video are disabled for this spec so its one-time key is
not retained in diagnostic artifacts. Missing harness isolation/DSN fails the test.

### Payment reference fixture

The same isolated harness starts an HTTPS-only reference cashier on loopback
(default port 18443; `CONDUIT_E2E_PAYMENT_PORT` overrides it). OpenSSL is required.
A fresh task-owned CA, server certificate and private keys are created inside the
run's temporary directory. Only the payment provider client receives that CA.
Chromium receives a single-run SHA256 SPKI pin under the isolated harness marker;
Playwright uses a separate temporary browser profile. System trust and global
certificate verification settings are unchanged. The fixture configuration, keys,
servers and journal/log files are removed by the harness on exit. `--check` only
checks prerequisites and free ports; it does not generate certificates or start
services.

This gateway implements only the pinned maajiko/Epay a4d0f042 classic-MD5
reference profile. It accepts hosted checkout, sends signed GET notifications
and answers order queries with the reference Shanghai-local endtime. No real
merchant, real provider or real money is used. Local simulated gateway success
does not establish compatibility with an unspecified merchant installation.
Never log full checkout, notification or query URLs: they contain signatures or
merchant keys. Payment browser tests disable trace, video and screenshots.

The suite also collects `group-pricing.spec.ts` (ordered group condition pricing,
complete draft save/reopen preservation, and unchanged published charge before
publication) and both channels in `payment-topup.spec.ts`. Each payment case
checks an unpaid return, frozen 1-fen/1.5x receipt, verified 150-credit top-up,
repeated reconciliation without duplicate credit, then a real normal-key local
upstream request charging exactly 4.2 credits in that same Project with one debit
and zero remaining reservation. Collection is not proof that these cases ran.
