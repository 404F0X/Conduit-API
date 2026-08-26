//! Atomic PostgreSQL API-key request quota admission.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct PgQuotaAdmission<'a> {
    pub api_key_id: i64,
    pub project_id: i64,
    pub profile_name: &'a str,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub end_inclusive: bool,
    pub request_limit: i64,
    pub admitted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgQuotaAdmissionOutcome {
    Admitted,
    Exceeded,
}

#[derive(Debug, Error)]
pub enum PgQuotaAdmissionError {
    #[error("postgres quota admission failed: {0}")]
    Database(#[from] sqlx::Error),
}

/// Serialize admissions for one API key across every process connected to the
/// same PostgreSQL cluster. Locking the key (rather than a currently-existing
/// ledger row) also protects the empty-window case where `FOR UPDATE` has no
/// row to lock.
pub async fn admit_postgres_request(
    pool: &PgPool,
    input: &PgQuotaAdmission<'_>,
) -> Result<PgQuotaAdmissionOutcome, PgQuotaAdmissionError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(input.api_key_id)
        .execute(&mut *tx)
        .await?;

    let mut count = QueryBuilder::<Postgres>::new(
        "SELECT COUNT(*) FROM api_key_quota_admissions WHERE api_key_id = ",
    );
    count.push_bind(input.api_key_id);
    count.push(" AND profile_name = ");
    count.push_bind(input.profile_name);
    if let Some(start) = input.start {
        count.push(" AND created_at >= ");
        count.push_bind(start);
    }
    if let Some(end) = input.end {
        count.push(if input.end_inclusive {
            " AND created_at <= "
        } else {
            " AND created_at < "
        });
        count.push_bind(end);
    }
    let admitted: i64 = count.build_query_scalar().fetch_one(&mut *tx).await?;
    if input.request_limit >= 0 && admitted >= input.request_limit {
        tx.rollback().await?;
        return Ok(PgQuotaAdmissionOutcome::Exceeded);
    }

    sqlx::query(
        "INSERT INTO api_key_quota_admissions \
         (api_key_id, project_id, profile_name, created_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(input.api_key_id)
    .bind(input.project_id)
    .bind(input.profile_name)
    .bind(input.admitted_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(PgQuotaAdmissionOutcome::Admitted)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;

    #[tokio::test]
    async fn concurrent_admissions_do_not_overshoot_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = Arc::new(database.pool.clone());
        let api_key_id = 9_000_000_000_i64 + i64::from(std::process::id());
        sqlx::query("DELETE FROM api_key_quota_admissions WHERE api_key_id = $1")
            .bind(api_key_id)
            .execute(pool.as_ref())
            .await?;
        let now = Utc::now();
        let mut tasks = Vec::new();
        for _ in 0..24 {
            let pool = pool.clone();
            tasks.push(tokio::spawn(async move {
                admit_postgres_request(
                    pool.as_ref(),
                    &PgQuotaAdmission {
                        api_key_id,
                        project_id: 1,
                        profile_name: "concurrency-test",
                        start: Some(now - chrono::Duration::minutes(1)),
                        end: Some(now + chrono::Duration::minutes(1)),
                        end_inclusive: false,
                        request_limit: 5,
                        admitted_at: now,
                    },
                )
                .await
            }));
        }
        let mut accepted = 0;
        for task in tasks {
            if task.await?? == PgQuotaAdmissionOutcome::Admitted {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 5);
        let persisted: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM api_key_quota_admissions WHERE api_key_id = $1",
        )
        .bind(api_key_id)
        .fetch_one(pool.as_ref())
        .await?;
        assert_eq!(persisted, 5);
        sqlx::query("DELETE FROM api_key_quota_admissions WHERE api_key_id = $1")
            .bind(api_key_id)
            .execute(pool.as_ref())
            .await?;
        database.cleanup().await?;
        Ok(())
    }

    /// Opt-in hotspot benchmark for one API key's atomic quota admission.
    ///
    /// Fixture population is outside the timed region. Each cardinality puts
    /// all concurrent attempts behind the same advisory lock and deliberately
    /// leaves room for only part of the workload, so the benchmark also proves
    /// that the persisted count cannot cross the configured request limit.
    #[tokio::test]
    async fn postgres_quota_admission_hotspot_benchmark_when_explicitly_enabled()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("CONDUIT_PG_BENCH").as_deref() != Ok("1") {
            return Ok(());
        }
        let dsn = std::env::var("CONDUIT_TEST_POSTGRES_DSN").map_err(|_| {
            std::io::Error::other("CONDUIT_TEST_POSTGRES_DSN is required when CONDUIT_PG_BENCH=1")
        })?;
        let operations = benchmark_usize("CONDUIT_PG_BENCH_OPERATIONS", 64)?;
        let concurrency = benchmark_usize("CONDUIT_PG_BENCH_CONCURRENCY", 8)?;
        let timeout_secs = benchmark_u64("CONDUIT_PG_BENCH_TIMEOUT_SECS", 120)?;
        if operations == 0 || concurrency == 0 || timeout_secs == 0 {
            return Err(std::io::Error::other(
                "CONDUIT_PG_BENCH_OPERATIONS, CONDUIT_PG_BENCH_CONCURRENCY, and CONDUIT_PG_BENCH_TIMEOUT_SECS must be positive",
            )
            .into());
        }

        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        for (scenario, prefill) in [0_usize, 1_000, 10_000].into_iter().enumerate() {
            run_hotspot_scenario(
                &database.pool,
                scenario,
                prefill,
                operations,
                concurrency,
                timeout_secs,
            )
            .await?;
        }
        database.cleanup().await?;
        Ok(())
    }

    fn benchmark_usize(name: &str, default: usize) -> Result<usize, std::io::Error> {
        std::env::var(name).map_or(Ok(default), |value| {
            value.parse().map_err(|error| {
                std::io::Error::other(format!("invalid {name} value {value:?}: {error}"))
            })
        })
    }

    fn benchmark_u64(name: &str, default: u64) -> Result<u64, std::io::Error> {
        std::env::var(name).map_or(Ok(default), |value| {
            value.parse().map_err(|error| {
                std::io::Error::other(format!("invalid {name} value {value:?}: {error}"))
            })
        })
    }

    async fn run_hotspot_scenario(
        pool: &PgPool,
        scenario: usize,
        prefill: usize,
        operations: usize,
        concurrency: usize,
        timeout_secs: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let api_key_id =
            9_100_000_000_i64 + i64::from(std::process::id()) * 10 + i64::try_from(scenario)?;
        let project_id = 1_i64;
        let profile_name = "quota-hotspot-benchmark";
        let now = Utc::now();

        // Generate the fixture in one statement. This setup is intentionally
        // excluded from admission latency and throughput measurements.
        sqlx::query(
            "INSERT INTO api_key_quota_admissions \
             (api_key_id,project_id,profile_name,created_at) \
             SELECT $1,$2,$3,$4 FROM generate_series(1,$5::BIGINT)",
        )
        .bind(api_key_id)
        .bind(project_id)
        .bind(profile_name)
        .bind(now)
        .bind(i64::try_from(prefill)?)
        .execute(pool)
        .await?;

        let admission_budget = operations.div_ceil(2);
        let request_limit = i64::try_from(
            prefill
                .checked_add(admission_budget)
                .ok_or_else(|| std::io::Error::other("benchmark request limit overflow"))?,
        )?;
        let limiter = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut tasks = tokio::task::JoinSet::new();
        let workload_started = Instant::now();
        for _ in 0..operations {
            let pool = pool.clone();
            let limiter = limiter.clone();
            tasks.spawn(async move {
                let _permit = limiter
                    .acquire_owned()
                    .await
                    .map_err(|error| error.to_string())?;
                let started = Instant::now();
                let outcome = admit_postgres_request(
                    &pool,
                    &PgQuotaAdmission {
                        api_key_id,
                        project_id,
                        profile_name,
                        start: Some(now - chrono::Duration::minutes(1)),
                        end: Some(now + chrono::Duration::minutes(1)),
                        end_inclusive: false,
                        request_limit,
                        admitted_at: now,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
                Ok::<_, String>((started.elapsed(), outcome))
            });
        }

        let collect = async {
            let mut latencies = Vec::with_capacity(operations);
            let mut accepted = 0_usize;
            while let Some(result) = tasks.join_next().await {
                let (latency, outcome) = result
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .map_err(std::io::Error::other)?;
                latencies.push(latency);
                accepted += usize::from(outcome == PgQuotaAdmissionOutcome::Admitted);
            }
            Ok::<_, Box<dyn std::error::Error>>((latencies, accepted))
        };
        let (mut latencies, accepted) = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            collect,
        )
        .await
        .map_err(|_| {
            std::io::Error::other(format!(
                "quota admission benchmark exceeded {timeout_secs}s at prefill={prefill}; possible lock stall"
            ))
        })??;
        let elapsed = workload_started.elapsed();
        latencies.sort_unstable();

        let persisted: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT FROM api_key_quota_admissions \
             WHERE api_key_id=$1 AND profile_name=$2 AND created_at >= $3 AND created_at < $4",
        )
        .bind(api_key_id)
        .bind(profile_name)
        .bind(now - chrono::Duration::minutes(1))
        .bind(now + chrono::Duration::minutes(1))
        .fetch_one(pool)
        .await?;
        let expected = i64::try_from(prefill + admission_budget)?;
        assert_eq!(latencies.len(), operations, "every attempt must complete");
        assert_eq!(accepted, admission_budget, "unexpected admitted count");
        assert_eq!(persisted, expected, "admission ledger count mismatch");
        assert_eq!(
            persisted, request_limit,
            "quota admission exceeded its limit"
        );

        println!(
            "postgres quota admission benchmark: prefill={prefill} attempts={operations} \
             concurrency={concurrency} admitted={accepted} exceeded={} throughput={:.2} attempts/s \
             p50={}us p95={}us p99={}us max={}us",
            operations - accepted,
            operations as f64 / elapsed.as_secs_f64(),
            percentile_micros(&latencies, 50),
            percentile_micros(&latencies, 95),
            percentile_micros(&latencies, 99),
            latencies.last().map_or(0, Duration::as_micros),
        );
        Ok(())
    }

    fn percentile_micros(latencies: &[Duration], percent: usize) -> u128 {
        let index = (latencies.len() * percent).div_ceil(100).saturating_sub(1);
        latencies[index].as_micros()
    }
}
