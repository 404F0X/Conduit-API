# Production Deployment

This is the supported first-release deployment shape for the Rust backend and
the retained frontend: one Conduit API container, one persistent PostgreSQL
container, and a TLS reverse proxy in front of the loopback-bound HTTP port.

## Current boundary

- PostgreSQL support is always compiled into the production image and source
  builds. No alternative writable business-database backend is supported.
- Run one application replica until distributed cache/session coordination and
  multi-replica acceptance tests are enabled for the deployment.
- The application image runs as UID/GID 10001, writes logs to stdout, and
  exposes application port 8090 plus internal metrics port 9090. PostgreSQL
  data is owned by the PostgreSQL container.
- Container health requires both the liveness endpoint and a database-backed
  system-status read to succeed.
- `compose.yml` publishes the application on `127.0.0.1:8090` by default. A
  reverse proxy should terminate TLS and forward to that address.

## Build and validate

The repository pins Rust 1.96.0 in both `rust-toolchain.toml` and the Docker
builder stage so local, CI, and image builds use the same compiler.

Tagged releases publish signed `linux/amd64` and `linux/arm64` images. The tag
must exactly match the workspace version, and publication occurs only after the
full CI and container smoke gates pass:

```sh
export TAG='v0.1.0-alpha.4'
export IMAGE='ghcr.io/404f0x/conduit-api:0.1.0-alpha.4'
docker pull "$IMAGE"
cosign verify \
  --certificate-identity "https://github.com/404F0X/Conduit-API/.github/workflows/publish-release.yml@refs/tags/$TAG" \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  "$IMAGE"
```

The corresponding GitHub Release contains the immutable manifest digest and
its checksum. SBOM and provenance attestations are attached to the image in
GHCR. Prefer deploying the recorded digest instead of a mutable tag. To use a
published image with the supported Compose model, set `CONDUIT_IMAGE` and
disable local building:

```sh
export CONDUIT_IMAGE='ghcr.io/404f0x/conduit-api@sha256:replace-with-release-digest'
docker compose pull conduit-api
docker compose up -d --no-build
```

```sh
export CONDUIT_POSTGRES_PASSWORD='replace-with-a-long-random-value'
docker compose config --quiet
docker compose build
docker compose up -d
docker compose ps
curl -fsS http://127.0.0.1:8090/health
curl -fsS http://127.0.0.1:8090/admin/system/status
```

Open `http://127.0.0.1:8090` through the intended reverse proxy and complete
first-owner initialization. There is no default application administrator
password, and the Compose file supplies no fallback database password.

For a direct source build without Compose, compile PostgreSQL support and set
the database block (or the equivalent `CONDUIT_DB_*` environment variables):

```yaml
db:
  dialect: postgres
  dsn: postgresql://conduit:replace-me@127.0.0.1:5432/conduit
  max_open_conns: 20
  max_idle_conns: 5
  connect_timeout: 30s
```

```sh
cargo build -p conduit-bin --bin conduit-api
./target/debug/conduit-api --config config.postgres.yml
```

Conduit API applies embedded schema migrations at startup unless
`disable_auto_migration` is enabled. When disabled, startup remains read-only
with respect to the schema but fails unless every embedded migration is already
recorded and the latest version exactly matches the running binary. Configured
read replicas are checked even in this mode; a stale replica either blocks
startup or is disabled when replica fallback is enabled. Legacy `.db` files are
unsupported and are not imported automatically; point the runtime at a
PostgreSQL database.

Before exposing a TLS deployment, set both values to its external origin (no
trailing slash):

```sh
CONDUIT_PUBLIC_URL=https://conduit.example.com
CONDUIT_CORS_ALLOWED_ORIGINS='["https://conduit.example.com"]'
CONDUIT_TRUSTED_PROXIES='["127.0.0.1"]'
```

`CONDUIT_PUBLIC_URL` is used for externally visible callback URLs.
`CONDUIT_CORS_ALLOWED_ORIGINS` prevents another website from using a browser to
call the unauthenticated first-owner initialization endpoint. The Compose
default accepts only its two local HTTP origins; do not set it to `*` in
production. Configure the reverse proxy to replace, rather than append to,
client-supplied `X-Forwarded-For`, `X-Forwarded-Proto`, and `X-Forwarded-Host`
headers. `CONDUIT_TRUSTED_PROXIES` must contain only the exact IP addresses or
CIDR ranges of reverse proxies that connect directly to Conduit API. With the
default empty list, all forwarding headers are ignored and the TCP peer is the
client address.

When several trusted proxies form a chain, each proxy must remove any
client-supplied forwarding fields and then append only the address it actually
observed. Conduit walks the chain from the trusted TCP peer toward the client
and uses the first untrusted address. Never add a broad client/network range to
the trusted list merely to make the displayed IP match; doing so restores the
header-spoofing risk that the trust boundary is intended to prevent.

To enable OIDC, pass provider configuration as JSON without putting it in the
image:

```sh
CONDUIT_OIDC_ENABLED=true
CONDUIT_OIDC_PROVIDERS='[{"name":"company","issuer_url":"https://idp.example.com","client_id":"conduit","client_secret":"replace-from-secret-store","scopes":["openid","profile","email"],"allow_signup":true}]'
```

The provider callback must allow
`https://conduit.example.com/oauth/oidc/callback` for a single configured
provider, or `/oauth/oidc/callback/{name}` when multiple providers are
configured. Supply the client secret through the deployment secret manager;
do not commit it to Compose or `.env`.

The channel-side Antigravity OAuth flow is disabled until both of its Google
OAuth credentials are injected. Configure them only when that provider flow is
needed:

```sh
CONDUIT_ANTIGRAVITY_CLIENT_ID='replace-from-secret-store'
CONDUIT_ANTIGRAVITY_CLIENT_SECRET='replace-from-secret-store'
```

An incomplete pair is rejected before an OAuth session is created. Keep both
values in the deployment secret manager; they are not compiled into Conduit.

Backups containing channels, API keys, or request logs are encrypted before
download or upload. Configure a dedicated random 32-byte key as base64 and
keep it outside the database and image:

```sh
CONDUIT_BACKUP_ENCRYPTION_KEY='<base64-encoded-32-byte-key>'
```

Sensitive backup creation fails closed when this key is absent or invalid.
Restore requires the same key; losing it makes encrypted archives
unrecoverable. Do not reuse the JWT, database, or provider secret.

Price restoration validates the archive before writing. Procurement-price
heads, price history, and retail price books must all use the archived
accounting currency. An archive that changes accounting currency can only be
restored into a database with no existing pricing state; use a disposable
restore target for cross-currency recovery or migration work.

To publish directly instead of using a local reverse proxy, explicitly set
`CONDUIT_HTTP_BIND=0.0.0.0`. Direct publication without TLS is not recommended.

## Database credentials and persistent data

Set `CONDUIT_POSTGRES_PASSWORD` through the deployment environment or secret
manager before every Compose command. The committed Compose file has no
fallback password and therefore fails closed when the value is absent. Because
the value is interpolated into a PostgreSQL URL, use URI-unreserved characters
(letters, digits, `-._~`) or replace the complete `CONDUIT_DB_DSN` entry in a
deployment-specific Compose override.

The reference Compose file uses environment interpolation, not Docker secrets.
Rendered Compose output and container environment metadata can therefore
contain the database password. Use `docker compose config --quiet` for
validation, restrict access to the deployment host, and have a secret manager
inject the value at deployment time; never commit it to Compose or `.env`.

The named volume `conduit-postgres-data` is mounted at
`/var/lib/postgresql/data`. Do not copy individual PostgreSQL data files while
the server is running. Use `pg_dump`/`pg_restore` for logical backups, or a
PostgreSQL-aware volume snapshot procedure. Test restore, not only backup
creation. There is no legacy `.db` dual-write or automatic import path in this
deployment.

The application-level JSON archive is a selective configuration/export
facility, not a financial disaster-recovery backup. It does not include the
Project wallet ledger, reservations, redemption-code state, or redemption
receipts. Recoverable Credit deployments must therefore back up and rehearse
restoring the complete PostgreSQL database.

If an administrator enables a filesystem-backed data storage from the UI,
configure its directory under a separately mounted persistent path. A path in
the container's writable layer is not durable; S3, GCS, or WebDAV storage does
not require an additional local volume.

## Upgrade and rollback

Before every upgrade:

1. Generate and verify a PostgreSQL logical backup.
2. Record the currently deployed immutable image tag or digest.
3. Take a PostgreSQL-consistent snapshot when the migration is high risk.
4. Build or pull the candidate image, then recreate the service.
5. Verify health, owner login, Admin GraphQL, one configured model request,
   metrics, and backup generation.

Rollback means restoring both the previous image and a database snapshot that
matches its schema. Rolling back only the image after an irreversible database
migration is unsafe.

## Required production acceptance

- Validate `docker compose build`, PostgreSQL startup ordering, health,
  restart, and volume persistence on the target Linux host.
- Configure TLS, request-size/time limits, trusted proxy headers, and CORS.
- Exercise backup and restore on a disposable copy of production-like data.
- Configure log collection, metrics scraping, alerts, and operational owners.
- Validate every enabled provider with real credentials in staging.
- Run a staging soak before routing production traffic.
