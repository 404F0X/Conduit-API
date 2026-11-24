# PostgreSQL migrations

This directory is Conduit API's only runtime migration catalog. The Rust database
crate embeds these files in version order and records applied versions in
`schema_migrations`.

The current catalog ends at `000039_model_price_formulas.sql`. The workspace is
still in its pre-release, rebuildable-database phase. The earlier `000028`
money-unit contract still requires recreation for databases that recorded its
superseded draft.

`000039` makes model display names nonunique while preserving unique public
keys, persists fixed-source retail formulas and their draft ownership, and
stores immutable admission pricing snapshots on wallet reservations. Source
configuration changes retire only associated formula drafts; published prices
and archived effective-time versions remain intact.

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

`000033` adds one-time Project credit redemption batches, digest-only codes,
immutable redemption receipts, and transaction-local audit rows. Plaintext
codes are returned once by the application and must never be persisted or
logged.

`000034` adds a bounded redemption limit to each batch and changes receipt
uniqueness to `(code_id, user_id)`. Existing batches default to one redemption,
while new batches may explicitly allow up to 100,000 distinct users per code.

`000035` adds expiring, idempotent API-key concurrency leases. Transaction-level
advisory locking makes the configured request limit consistent across replicas.

After this contract is released, schema changes must add the next numbered file
and update the embedded catalog in `crates/conduit-db/src/migrate.rs`.

Live migration tests use an isolated schema and require an explicit
`CONDUIT_TEST_POSTGRES_DSN`. No production database is inferred.

`000040_user_model_groups.sql` leaves existing users and Keys unassigned intentionally. Alpha operators must explicitly configure membership, grants and Key ModelGroup assignments before public requests are accepted; Project routing/accounting data remains unchanged.

`000041_payment_topups.sql` adds immutable Project payment orders, append-only sanitized events and dedicated one-per-order `payment_topup` ledger constraints, plus independent UserGroup recharge PPM/revision. It reuses the existing wallet balance trigger and does not alter grants, codes or request settlement. Payments default disabled; see backend knowledge for the explicit reference profile and configuration.
