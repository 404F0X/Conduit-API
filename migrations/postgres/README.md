# PostgreSQL migrations

This directory is Conduit API's only runtime migration catalog. The Rust database
crate embeds these files in version order and records applied versions in
`schema_migrations`.

The current catalog ends at `000032_change_sets.sql`. The workspace is
still in its pre-release, rebuildable-database phase. The earlier `000028`
money-unit contract still requires recreation for databases that recorded its
superseded draft.

Fresh schemas default retail price books to the real-world accounting currency
`CNY`; imported channel prices must provide an explicit three-uppercase-letter
currency code. Project wallets, credit accounts, subscription plans, customer
charge events, and project commercial profiles use the stable ledger code
`STATION_CREDIT`. A configurable display name belongs in application settings,
not database currency identifiers.

`000029` stores only SHA-256 affinity-key digests and credential fingerprints.
Raw prompt-cache keys, response ids, prompts, and provider credentials are not
part of the schema. An indexed expiry column supports the runtime's bounded
background cleanup.

`000030` keeps upstream price observations in channel balance units and stores
the channel billing metadata, accounting rate version, converted values, or a
conversion error beside each immutable observation.

`000031` adds an append-only price-change audit stream. Successful price
changes and their audit rows are committed in the same transaction.

`000032` adds a review queue for upstream price changes. Synchronization can
create or supersede drafts, but only an explicit approval transaction may
update formal channel procurement prices and their immutable versions.

After this contract is released, schema changes must add the next numbered file
and update the embedded catalog in `crates/conduit-db/src/migrate.rs`.

Live migration tests use an isolated schema and require an explicit
`CONDUIT_TEST_POSTGRES_DSN`. No production database is inferred.
