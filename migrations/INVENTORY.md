# PostgreSQL Migration Inventory

Status: active runtime inventory. PostgreSQL is the only supported database and
`migrations/postgres/` is the only migration tree used by builds, tests, and
deployments.

## Supported boundary

- Fresh PostgreSQL databases are migrated at startup unless
  `db.disable_auto_migration` is enabled.
- This pre-release workspace currently supports fresh-schema recreation; the
  experimental `000028` contract intentionally has no compatibility upgrade.
- Legacy `.db` files are unsupported and are not imported automatically.
- Migration behavior is defined by the embedded PostgreSQL SQL files and the
  repository integration tests in `conduit-db`.

## Current migration groups

- `000001_initial.sql`: base Conduit API schema.
- `000002`–`000009`: commercialization, balances, subscriptions, wallet shadow,
  and commercial audit foundations.
- `000011`–`000018`: simple groups, quota/provider observations, and API-key
  quota admission.
- `000019`–`000022`: subscription snapshots and request-routing/credential
  identity records.
- `000023`–`000025`: PostgreSQL performance indexes and index cleanup.
- `000026`–`000028`: wallet balance snapshots, usage-charge outbox, and the
  real-world accounting-currency/station-credit model.
- `000029`: hashed explicit route-affinity feedback and affinity decision
  fields on request route explanations.
- `000030`: immutable channel-unit price observations with snapshotted
  accounting-currency conversion inputs and results.
- `000031`: append-only pricing change audits with actor, before/after
  snapshots, accounting-rate version, and request correlation.
- `000032`: reviewable provider-price drafts between immutable upstream
  observations and formal procurement-price heads/history.

Numeric gaps are historical and must not be filled by renaming an already
released migration. New migrations use the next unused monotonically increasing
prefix.

## Required evidence before release

- [ ] Fresh-database migration succeeds on the supported PostgreSQL version.
- [ ] The development database is recreated when an experimental migration is
      rewritten before release.
- [ ] Repository integration tests pass with `CONDUIT_TEST_POSTGRES_DSN`.
- [ ] Backup and restore are rehearsed against a production-like database.
- [ ] Startup and rollback behavior are verified against a production-like
      PostgreSQL instance.

## Verification

```sh
bash scripts/db/verify_migrations_layout.sh
export CONDUIT_TEST_POSTGRES_DSN='postgresql://conduit:password@127.0.0.1:5432/conduit_test'
cargo test -p conduit-db
```

SQL files must match `NNNNNN_descriptive_name.sql` and must not contain
placeholder/template content. Keep operational migration notes in
`migrations/postgres/README.md`.
