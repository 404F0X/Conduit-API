# PostgreSQL Performance Baseline

> Baseline captured 2026-08-16 from the local PostgreSQL 18.4 instance configured by `.codex-runtime/pg-config.yml`.
> The DSN is intentionally not recorded in this document.

## Runtime

| Item | Value |
|---|---:|
| PostgreSQL | 18.4 |
| `max_connections` | 100 |
| `shared_buffers` | 128MB |
| `work_mem` | 4MB |
| `effective_cache_size` | 4GB |
| Conduit API configured `max_open_conns` | 10 |
| Conduit API configured `max_idle_conns` | 2 |
| `requests` rows | 173 |
| `request_executions` rows | 176 |
| `usage_logs` rows | 173 |

## Findings

1. The running database is now at schema `000027`; the wallet snapshot and durable settlement-outbox migrations are applied. Startup validates a configured read replica against this version before allowing it to serve reads.
2. The project request list query (`project_id` plus newest `created_at`) still uses the single-column `requests_by_created_at` index at this small scale and filters `project_id` afterward. The composite `(project_id, created_at DESC)` index remains available for larger cardinalities.
3. The 7-day Usage aggregation currently uses a sequential scan at 173 rows. This is acceptable at the current size, but the production access path needs a time-leading composite index for the Operations grouping/filter shape.
4. The PostgreSQL pool now uses a bounded statement cache, UTC session initialization, and `application_name = 'conduit'`. Operations and Dashboard explicitly use the read pool with operation-level fallback; strong-consistency services remain on master.

## First optimization slice

On 2026-08-16/17 the runtime database was migrated from `000020` through the performance cleanup sequence. The `000023` migration added candidate composite indexes for request status lists, execution health/retry lookups, and Operations time-window aggregations. Representative 100k-row plans showed that PostgreSQL used the existing narrow request/Usage indexes instead of three wider candidates. Migration `000024` removes the redundant request index and `000025` removes the two unhelpful Usage indexes, avoiding unproven write and VACUUM overhead. The PostgreSQL connection builder uses a bounded 256-entry statement cache and initializes each session with UTC and `application_name = 'conduit'`.

The same small-data query samples measured approximately 0.15–0.23 ms execution time before/after. PostgreSQL continued to choose a sequential scan for the 173-row Usage table and the existing newest-first request index; this is expected at this scale and is not treated as a production performance win. The representative Operations, wallet, quota, and lock-contention baselines below now cover the previously missing scale evidence.

## Before/after acceptance

The PostgreSQL mainline must record the same plans after each optimization:

- project request list: `project_id = ? ORDER BY created_at DESC LIMIT ?`
- execution health: `channel_id + model_id + credential_identity + created_at window`
- Usage/Operations: `created_at window` grouped by project/channel/model
- wallet ledger/reservations: account/project plus status and time window

Acceptance is based on PostgreSQL plan shape and measured p95/throughput.

## Opt-in settlement contention benchmark

The PostgreSQL settlement path has an opt-in benchmark that runs concurrent
transactions against one wallet and subscription bucket. It reports throughput,
p50, p95, p99, maximum transaction latency, maximum observed lock waiters, the
ratio of samples containing at least one lock waiter, and the database-wide
`pg_stat_database.deadlocks` delta. The test fails on PostgreSQL deadlock/lock-
timeout errors, a workload timeout, overdraw, duplicate/missing settlements, or
a funds-conservation mismatch.

```powershell
$env:CONDUIT_PG_BENCH='1'
$env:CONDUIT_TEST_POSTGRES_DSN='<postgres DSN>'
cargo test -p conduit-bin postgres_concurrent_settlement_benchmark_when_explicitly_enabled -- --nocapture
```

Optional workload controls are `CONDUIT_PG_BENCH_OPERATIONS` (default `64`),
`CONDUIT_PG_BENCH_CONCURRENCY` (default `8`), and
`CONDUIT_PG_BENCH_TIMEOUT_SECS` (default `120`). Lock waits are sampled every
`CONDUIT_PG_BENCH_SAMPLE_MS` milliseconds (default `10`). Each benchmark
transaction uses a unique transaction-local `application_name`, so lock samples
exclude other Conduit API and concurrent benchmark traffic. The waiter-sample ratio
is `samples with one or more Lock waiters / successful samples`; it is not the
fraction of transaction time spent waiting. The deadlock counter is database-
wide, so unrelated activity in the same database can increase its delta.

Fixture creation is excluded from timing. If the test role cannot read a
statistics view, settlement correctness and timing still run and the affected
diagnostic is printed as unavailable. This is a regression/diagnostic benchmark,
not a Go-parity claim or a production capacity result.

Local PostgreSQL 18.4 result on 2026-08-17 (debug test binary, one wallet and
subscription bucket):

| Operations | Concurrency | Throughput | p50 | p95 | p99/max | Max lock waiters | Waiter sample ratio | Deadlocks delta | Result |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 64 | 8 | 83.04 tx/s | 2.915 ms | 474.090 ms | 768.244 ms | 2 | 9.72% (7/72) | 0 | 64 settled; no overdraw, timeout, or deadlock |

The high tail relative to p50 is expected from deliberate contention on one
wallet row and is now a baseline to compare against future transaction changes.

## End-to-end hot-wallet admission benchmark

On 2026-08-17 a 300-request/300-concurrency proxy test exposed a connection-pool
convoy that the transaction-only benchmark could not show. With all requests
charging the same Project wallet, increasing the pool from 10 to 40 allowed
many transactions to occupy connections while waiting for the same wallet row;
only 155 requests succeeded and 145 returned 502. PostgreSQL itself was not at
its CPU or connection limit.

The runtime now queues the same `(project_id, currency)` before acquiring a
database connection. PostgreSQL `FOR UPDATE` remains the authoritative
cross-process lock. The hot transaction also avoids an unconditional wallet
upsert, combines ledger/reservation availability reads, and carries the known
settlement result into reservation capture instead of reading it back.

Local results below use the debug binary, three deterministic mock upstreams,
one deliberately hot wallet, and 300 simultaneous non-stream requests. They
are regression evidence, not production capacity promises; repeated 250ms
runs varied enough that the final pool-10 run is reported alongside the pool-40
comparison.

| Pool | Mock delay | Before admission | After admission + shorter transaction | Result |
|---:|---:|---:|---:|---|
| 40 | 250 ms | 155/300 succeeded; run reported about 127 req/s | 300/300; 90.0 req/s | connection-pool collapse removed |
| 40 | 0 ms | not captured | 300/300; 127.2 req/s | exposes application/database hot-wallet ceiling |
| 10 | 250 ms | 300/300; about 114.0 req/s | 300/300; final repeat 115.9 req/s | safe default retained |
| 10 | 0 ms | not captured | 300/300; 117.9 req/s | upstream delay removed |

Across the final pressure window, 3,001 requests produced exactly 3,001 usage
logs, customer charge events, and settlements. Open reservations and duplicate
usage charges were both zero. The remaining limit is the intentional
single-wallet strong-consistency boundary; multi-Project throughput should be
measured separately before changing that accounting model.

## Wallet snapshots and durable asynchronous settlement

Migrations `000026` and `000027` remove two additional hot-path costs without
weakening the accounting contract. PostgreSQL triggers maintain
`project_wallets.credit_balance_micros` and `credit_reserved_micros` from the
append-only credit ledger and reservation lifecycle, so admission and capture
read a locked wallet row in O(1) instead of aggregating its full history.
After Usage is durable, settlement first writes a durable outbox row and then
hands the job to a bounded queue (8 workers by default). The HTTP response no
longer waits for Capture/settlement, while the outbox remains the recovery
authority if the process exits before a worker finishes.

Local PostgreSQL 18.4 results on 2026-08-17, pool 10, one hot wallet, 300
simultaneous non-stream requests:

| Mock delay | Before snapshots/outbox | After snapshots/outbox | Result |
|---:|---:|---:|---|
| 250 ms | 115.9 req/s | 163.7 req/s | 300/300 HTTP success |
| 0 ms | 117.9 req/s | 205.4 req/s | 300/300 HTTP success |

The crash test sent 300 successful requests with one settlement worker and
then forcibly terminated the process. At termination, 282 Usage rows had
durable pending outbox rows and open reservations while 18 were settled.
Starting the updated binary drained all batches without manual repair:
charges advanced from 18 to 300, and pending outbox rows and open reservations
both reached zero. The wallet snapshot audit reported zero mismatches.

After the recovery run and final benchmarks, all 6,431 requests had exactly one
Usage row, charge event, and settlement; pending outbox rows, open reservations,
and snapshot mismatches were all zero. Focused real-PostgreSQL settlement tests
passed 11/11 and migration/catalog tests passed 39/39. This validates
exactly-once recovery and funds conservation for the tested failure point; it
does not replace a multi-instance or long-duration soak test.

## Opt-in wallet query-plan benchmark

The production settlement/admission wallet reads now have a separate query-plan
baseline. It loads 1,000 and then 10,000 append-only credit ledger rows plus the
same number of reservations and allocations into an isolated schema. Rows are
distributed over 100 wallet identities; the measured wallet therefore selects
1% of each table. Fixture insertion and `ANALYZE` are outside the measured
queries.

The benchmark executes the exact ledger-balance and active project-credit
reservation aggregate shapes used by admission and `settle_funds`. It records
execution time, shared hit/read blocks, node types, and selected indexes. It
also verifies the aggregate results, rejects temporary-block spills and
unnecessary sort nodes, requires the existing wallet/reservation indexes to be
selected, and applies a deliberately broad five-second regression ceiling. It
does not add or recommend an index merely from synthetic timings.

```powershell
$env:CONDUIT_PG_BENCH='1'
$env:CONDUIT_TEST_POSTGRES_DSN='<postgres DSN>'
cargo test -p conduit-bin postgres_wallet_ledger_and_reservation_plans_when_explicitly_enabled -- --nocapture
```

The test performs no database work unless both opt-in variables are supplied.
Its isolated schema is dropped after a successful run. Treat the printed plans
as a regression baseline, not as a production capacity claim.

Local PostgreSQL 18.4 result on 2026-08-17 (debug test binary, warm shared
buffers, target wallet selecting 1% of rows):

| Rows per table | Query | Execution | Selected access path | Result |
|---:|---|---:|---|---|
| 1,000 | credit ledger balance | 0.054 ms | `project_credit_ledger_entries_wallet` bitmap scan | exact sum; no spill/sort |
| 1,000 | admission reservations | 0.117 ms | reservation wallet/status + allocation-source index nested loop | exact sum; no spill/sort |
| 1,000 | settlement reservations | 0.052 ms | reservation wallet/status + allocation-source index nested loop | exact sum; no spill/sort |
| 10,000 | credit ledger balance | 0.109 ms | `project_credit_ledger_entries_wallet` bitmap scan | exact sum; no spill/sort |
| 10,000 | admission reservations | 0.458 ms | reservation wallet/status + allocation-source index nested loop | exact sum; no spill/sort |
| 10,000 | settlement reservations | 0.388 ms | reservation wallet/status + allocation-source index nested loop | exact sum; no spill/sort |

These plans validate the existing indexes at this scale; no wallet index change
is justified by this sample.

## Opt-in quota admission hotspot benchmark

The PostgreSQL API-key request-quota path also has an opt-in contention
benchmark. It runs the same-key atomic admission transaction after pre-filling
the current quota window with 0, 1,000, and 10,000 admissions. Fixture insertion
is excluded from timing. Each scenario reports attempt throughput and p50/p95/p99
latency, admits only half of the concurrent attempts, and verifies both the exact
ledger count and that the configured request limit was not crossed.

```powershell
$env:CONDUIT_PG_BENCH='1'
$env:CONDUIT_TEST_POSTGRES_DSN='<postgres DSN>'
cargo test -p conduit-db postgres_quota_admission_hotspot_benchmark_when_explicitly_enabled -- --nocapture
```

The shared optional workload controls are `CONDUIT_PG_BENCH_OPERATIONS`
(default `64`), `CONDUIT_PG_BENCH_CONCURRENCY` (default `8`), and
`CONDUIT_PG_BENCH_TIMEOUT_SECS` (default `120`). The test does no benchmark work
unless explicitly enabled.

Local PostgreSQL 18.4 result on 2026-08-17 (64 attempts, concurrency 8):

| Existing window rows | Throughput | p50 | p95 | p99/max | Result |
|---:|---:|---:|---:|---:|---|
| 0 | 118.88 attempts/s | 0.814 ms | 141.321 ms | 537.236 ms | 32 admitted / 32 exceeded; cold first scenario |
| 1,000 | 1,548.54 attempts/s | 5.006 ms | 8.815 ms | 10.512 ms | 32 admitted / 32 exceeded |
| 10,000 | 606.22 attempts/s | 12.012 ms | 16.743 ms | 18.664 ms | 32 admitted / 32 exceeded |

The first scenario includes cold connection/cache effects, so it is not a
same-condition throughput comparison. The 1,000→10,000 growth does show the
current lock-held `COUNT(*)` path becoming more expensive; an atomic window
counter should be considered before allowing very large per-key windows.

## Opt-in Operations query-plan benchmark

The PostgreSQL Operations adapter has a representative, isolated-schema plan
benchmark for the SQL actually used by its channel attempt, route-health, and
Usage aggregates. The production code and benchmark share the same SQL
constants, which prevents a synthetic benchmark query from drifting away from
the GraphQL ledger path. The fixture defaults to 20,000 executions, 10,000
Usage rows, 16 channels, multiple models and credential fingerprints, and a mix
of completed, failed, and retried requests. Twenty percent of executions are
placed in the route-health query's recent 15-minute window; the remainder spans
14 days. Fixture creation and `ANALYZE` are excluded from the full-ledger timing.

```powershell
$env:CONDUIT_PG_BENCH='1'
$env:CONDUIT_TEST_POSTGRES_DSN='<postgres DSN>'
cargo test -p conduit-bin postgres_operations_representative_plan_benchmark_when_explicitly_enabled -- --ignored --nocapture
```

Optional controls are `CONDUIT_PG_BENCH_OPERATIONS_ROWS` (default `20000`,
maximum `250000`), `CONDUIT_PG_BENCH_OPERATIONS_CHANNELS` (default `16`,
maximum `128`), and `CONDUIT_PG_BENCH_OPERATIONS_MAX_MS` (default `10000` per
representative query). The complete ledger receives four times the per-query
budget because it executes the remaining Operations queries as well.

For each representative SQL statement the test records planning/execution
milliseconds, node types, selected indexes, relations, shared hit/read blocks,
and temporary blocks from `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`. It fails if
the expected fact table is absent, the controlled fixture spills temporary
blocks, the duration exceeds its explicit budget, or the complete ledger does
not return the injected channel/attempt data. It intentionally does not require
a named index: PostgreSQL may correctly choose a sequential scan for a window
covering much of a small table. Add or remove indexes only after comparing these
measured plans at the intended production cardinality.

Local PostgreSQL 18.4 result on 2026-08-17 (debug test binary, default fixture):

| Scope | Execution | Planning | Shared hit/read blocks | Temporary writes | Observed access path |
|---|---:|---:|---:|---:|---|
| Complete Operations ledger | 165.701 ms | — | — | — | all production ledger queries |
| Attempt aggregate | 35.496 ms | 0.289 ms | 156,318 / 0 | 0 | channel/time index plus request/channel/status lookup index |
| Route health | 7.203 ms | 0.568 ms | 2,262 / 0 | 0 | channel/time bitmap index scan |
| Usage aggregate | 3.631 ms | 0.168 ms | 466 / 0 | 0 | sequential scan over the selected 7-day-heavy fixture |

All statements stayed inside the 10-second diagnostic budget and returned the
expected rows without temporary-file spill. The attempt aggregate's correlated
retry check produces substantially more buffer hits than the other two plans,
so it is the first query to remeasure at larger cardinality. Its current 35.496
ms execution does not justify adding another index. The Usage sequential scan
is also retained: roughly half of this controlled table falls inside the
window, making that planner choice reasonable.
