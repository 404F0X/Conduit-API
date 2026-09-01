//! PostgreSQL implementation of bounded-use Project credit redemption codes.
//!
//! Plaintext codes exist only in the creation response. Every database lookup
//! uses a normalized SHA-256 digest, and every successful state change writes
//! its audit row in the same transaction as the business data.

use async_graphql::ID;
use chrono::{DateTime, Utc};
use conduit_admin_graphql::billing as gql;
use conduit_auth::apikey::generate_api_key;
use conduit_core::objects::money::STATION_CREDIT_CODE;
use conduit_services::billing::micros_to_decimal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow, types::Json};

const REDEMPTION_CODE_PREFIX: &str = "conduit-credit";
const REDEMPTION_HINT_CHARS: usize = 8;
const STORAGE_ERROR: &str = "credit redemption storage operation failed";

pub(crate) async fn list_codes(
    pool: &PgPool,
    limit: i32,
    offset: i32,
) -> Result<gql::CreditRedemptionCodePage, gql::BillingError> {
    gql::validate_credit_redemption_pagination(limit, offset)?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| sanitized_db_error("list_codes.begin", error))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(|error| sanitized_db_error("list_codes.snapshot", error))?;
    let total = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM credit_redemption_codes")
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| sanitized_db_error("list_codes.count", error))?;
    let total = i32::try_from(total).map_err(|_| {
        internal_storage_error("list_codes.count", "row count exceeds GraphQL Int range")
    })?;
    let rows = sqlx::query(
        "SELECT c.id,c.batch_id,c.code_hint,c.status,c.redeemed_at,c.revoked_at,c.created_at, \
                b.amount_micros,b.currency,b.description,b.expires_at,b.max_redemptions, \
                (SELECT COUNT(*)::INTEGER FROM credit_redemption_receipts r WHERE r.code_id=c.id) AS redemption_count, \
                transaction_timestamp() AS read_at \
         FROM credit_redemption_codes c \
         JOIN credit_redemption_batches b ON b.id=c.batch_id \
         ORDER BY c.created_at DESC,c.id DESC LIMIT $1 OFFSET $2",
    )
    .bind(i64::from(limit))
    .bind(i64::from(offset))
    .fetch_all(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("list_codes.fetch", error))?;
    let items = rows.iter().map(code_from_row).collect();
    tx.commit()
        .await
        .map_err(|error| sanitized_db_error("list_codes.commit", error))?;
    Ok(gql::CreditRedemptionCodePage {
        items,
        total,
        limit,
        offset,
    })
}

pub(crate) async fn create_codes(
    pool: &PgPool,
    actor: gql::CreditRedemptionActor,
    input: gql::CreateCreditRedemptionCodesInput,
) -> Result<gql::CreateCreditRedemptionCodesPayload, gql::BillingError> {
    gql::validate_create_credit_redemption_codes_input(&input)?;
    let amount_micros = parse_exact_amount_micros(&input.amount)?;
    let expires_at = input.expires_at.as_deref().map(parse_expiry).transpose()?;
    let description = input
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let quantity = input.quantity;
    let max_redemptions = input.max_redemptions;
    validate_batch_liability(amount_micros, quantity, max_redemptions)?;

    // Generate outside the transaction so entropy acquisition never extends
    // the duration of database locks. Only normalized digests enter SQL.
    let generated = (0..quantity)
        .map(|_| {
            let plaintext = generate_code()?;
            let normalized = gql::normalize_credit_redemption_code(&plaintext).map_err(|_| {
                internal_storage_error(
                    "create_codes.generate",
                    "generated redemption code failed its own format contract",
                )
            })?;
            Ok((plaintext, digest_code(&normalized), code_hint(&normalized)))
        })
        .collect::<Result<Vec<_>, gql::BillingError>>()?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|error| sanitized_db_error("create_codes.begin", error))?;
    let created_at = transaction_time(&mut tx, "create_codes.time").await?;
    // Expiry validation is repeated against the transaction clock so a value
    // that elapsed while the request waited cannot create an already-expired
    // batch.
    if expires_at.is_some_and(|expiry| expiry <= created_at) {
        return Err(gql::BillingError::Invalid(
            "expiresAt must be in the future".to_string(),
        ));
    }
    let batch_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO credit_redemption_batches \
         (amount_micros,currency,quantity,max_redemptions,expires_at,description,created_by_actor_type,created_by_actor_id,created_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",
    )
    .bind(amount_micros)
    .bind(STATION_CREDIT_CODE)
    .bind(quantity)
    .bind(max_redemptions)
    .bind(expires_at)
    .bind(description.as_deref())
    .bind(&actor.actor_type)
    .bind(actor.actor_id.as_deref())
    .bind(created_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("create_codes.insert_batch", error))?;

    let mut codes = Vec::with_capacity(generated.len());
    for (plaintext, digest, hint) in generated {
        let code_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO credit_redemption_codes \
             (batch_id,code_digest,code_hint,status,created_at) \
             VALUES($1,$2,$3,'active',$4) RETURNING id",
        )
        .bind(batch_id)
        .bind(digest)
        .bind(&hint)
        .bind(created_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| sanitized_db_error("create_codes.insert_code", error))?;
        codes.push(gql::GeneratedCreditRedemptionCode {
            id: id(code_id),
            code: plaintext,
            code_hint: hint,
        });
    }
    insert_audit(
        &mut tx,
        &actor,
        AuditRow {
            operation: "create_codes",
            batch_id,
            code_id: None,
            receipt_id: None,
            project_id: None,
            user_id: None,
            outcome: "success",
            detail: json!({
                "amountMicros": amount_micros,
                "currency": STATION_CREDIT_CODE,
                "quantity": quantity,
                "maxRedemptions": max_redemptions,
                "expiresAt": expires_at.map(|value| value.to_rfc3339()),
            }),
            created_at,
        },
    )
    .await?;
    tx.commit()
        .await
        .map_err(|error| sanitized_db_error("create_codes.commit", error))?;

    Ok(gql::CreateCreditRedemptionCodesPayload {
        batch_id: id(batch_id),
        amount: amount(amount_micros),
        currency: STATION_CREDIT_CODE.to_string(),
        quantity,
        max_redemptions,
        expires_at: expires_at.map(wire_time),
        codes,
    })
}

pub(crate) async fn revoke_code(
    pool: &PgPool,
    actor: gql::CreditRedemptionActor,
    code_id: &str,
) -> Result<gql::CreditRedemptionCode, gql::BillingError> {
    let code_id = parse_id(code_id, "redemption code ID")?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| sanitized_db_error("revoke_code.begin", error))?;
    let row = sqlx::query(
        "SELECT c.id,c.batch_id,c.code_hint,c.status,c.redeemed_at,c.revoked_at,c.created_at, \
                b.amount_micros,b.currency,b.description,b.expires_at,b.max_redemptions, \
                (SELECT COUNT(*)::INTEGER FROM credit_redemption_receipts r WHERE r.code_id=c.id) AS redemption_count \
         FROM credit_redemption_codes c \
         JOIN credit_redemption_batches b ON b.id=c.batch_id \
         WHERE c.id=$1 FOR UPDATE OF c",
    )
    .bind(code_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("revoke_code.lock", error))?
    .ok_or_else(|| gql::BillingError::NotFound("credit redemption code".to_string()))?;
    // Use the wall clock after any row-lock wait; transaction_timestamp()
    // would retain a pre-wait instant and could revive a code that expired
    // while a concurrent redemption held the lock.
    let revoked_at = wall_clock_time(&mut tx, "revoke_code.time").await?;
    let status = row.get::<String, _>("status");
    let expiry = row.get::<Option<DateTime<Utc>>, _>("expires_at");
    if status != "active" || expiry.is_some_and(|value| value <= revoked_at) {
        return Err(gql::BillingError::Invalid(
            "credit redemption code is not active".to_string(),
        ));
    }
    sqlx::query(
        "UPDATE credit_redemption_codes \
         SET status='revoked',revoked_at=$2 WHERE id=$1 AND status='active'",
    )
    .bind(code_id)
    .bind(revoked_at)
    .execute(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("revoke_code.update", error))?;
    let batch_id = row.get::<i64, _>("batch_id");
    insert_audit(
        &mut tx,
        &actor,
        AuditRow {
            operation: "revoke_code",
            batch_id,
            code_id: Some(code_id),
            receipt_id: None,
            project_id: None,
            user_id: None,
            outcome: "success",
            detail: json!({}),
            created_at: revoked_at,
        },
    )
    .await?;
    tx.commit()
        .await
        .map_err(|error| sanitized_db_error("revoke_code.commit", error))?;

    Ok(gql::CreditRedemptionCode {
        id: id(code_id),
        batch_id: id(batch_id),
        code_hint: row.get("code_hint"),
        amount: amount(row.get("amount_micros")),
        currency: row.get("currency"),
        description: row.get("description"),
        max_redemptions: row.get("max_redemptions"),
        redemption_count: row.get("redemption_count"),
        remaining_redemptions: row
            .get::<i32, _>("max_redemptions")
            .saturating_sub(row.get("redemption_count")),
        status: gql::CreditRedemptionCodeStatus::Revoked,
        expires_at: expiry.map(wire_time),
        redeemed_at: row
            .get::<Option<DateTime<Utc>>, _>("redeemed_at")
            .map(wire_time),
        revoked_at: Some(wire_time(revoked_at)),
        created_at: wire_time(row.get("created_at")),
    })
}

pub(crate) async fn redeem_code(
    pool: &PgPool,
    actor: gql::CreditRedemptionActor,
    user_id: &str,
    project_id: &str,
    code: &str,
) -> Result<gql::CreditRedemptionReceipt, gql::BillingError> {
    let user_id = parse_id(user_id, "user ID")?;
    ensure_actor_matches_user(&actor, user_id)?;
    let project_id = parse_id(project_id, "project ID")?;
    let normalized = gql::normalize_credit_redemption_code(code)?;
    let digest = digest_code(&normalized);
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| sanitized_db_error("redeem_code.begin", error))?;
    // Lock all three authorization records until commit. This prevents a
    // membership removal, user deactivation, or Project archive from racing
    // between authorization and the credit write.
    ensure_active_membership_tx(&mut tx, user_id, project_id).await?;
    let code_row = sqlx::query(
        "SELECT c.id,c.batch_id,c.status,b.amount_micros,b.currency,b.expires_at,b.max_redemptions \
         FROM credit_redemption_codes c \
         JOIN credit_redemption_batches b ON b.id=c.batch_id \
         WHERE c.code_digest=$1 FOR UPDATE OF c",
    )
    .bind(digest)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("redeem_code.lock_code", error))?
    .ok_or(gql::BillingError::RedemptionCodeUnavailable)?;
    // Decide expiry only after acquiring the code lock. A waiting transaction
    // must not reuse its older transaction-start timestamp.
    let redeemed_at = wall_clock_time(&mut tx, "redeem_code.time").await?;
    let code_id = code_row.get::<i64, _>("id");
    let batch_id = code_row.get::<i64, _>("batch_id");
    let status = code_row.get::<String, _>("status");
    let expires_at = code_row.get::<Option<DateTime<Utc>>, _>("expires_at");

    // Replay lookup precedes terminal-state and expiry checks. Once this user
    // has redeemed the code, the original Project receives the same receipt
    // even if the code was later exhausted, revoked, or expired. The unique
    // (code_id, user_id) constraint prevents Project switching from consuming
    // another slot.
    let replay_receipt = sqlx::query(
        "SELECT id,code_id,project_id,user_id,amount_micros,currency,redeemed_at \
         FROM credit_redemption_receipts WHERE code_id=$1 AND user_id=$2",
    )
    .bind(code_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("redeem_code.replay_receipt", error))?;
    if let Some(receipt_row) = replay_receipt {
        if receipt_row.get::<i64, _>("project_id") != project_id {
            return Err(gql::BillingError::RedemptionCodeUnavailable);
        }
        let receipt_id = receipt_row.get::<i64, _>("id");
        insert_audit(
            &mut tx,
            &actor,
            AuditRow {
                operation: "redeem_code",
                batch_id,
                code_id: Some(code_id),
                receipt_id: Some(receipt_id),
                project_id: Some(project_id),
                user_id: Some(user_id),
                outcome: "replayed",
                detail: json!({}),
                created_at: redeemed_at,
            },
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| sanitized_db_error("redeem_code.replay_commit", error))?;
        return Ok(receipt_from_row(&receipt_row));
    }
    if status != "active" || expires_at.is_some_and(|expiry| expiry <= redeemed_at) {
        return Err(gql::BillingError::RedemptionCodeUnavailable);
    }
    let redemption_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM credit_redemption_receipts WHERE code_id=$1",
    )
    .bind(code_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("redeem_code.count_receipts", error))?;
    let max_redemptions = i64::from(code_row.get::<i32, _>("max_redemptions"));
    if redemption_count >= max_redemptions {
        return Err(gql::BillingError::RedemptionCodeUnavailable);
    }
    let updated_redemption_count = redemption_count + 1;

    sqlx::query(
        "INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) \
         VALUES($1,$2,'active',$3,$3) ON CONFLICT(project_id,currency) DO NOTHING",
    )
    .bind(project_id)
    .bind(STATION_CREDIT_CODE)
    .bind(redeemed_at)
    .execute(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("redeem_code.create_wallet", error))?;
    let wallet = sqlx::query(
        "SELECT id,status FROM project_wallets \
         WHERE project_id=$1 AND currency=$2 FOR UPDATE",
    )
    .bind(project_id)
    .bind(STATION_CREDIT_CODE)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("redeem_code.lock_wallet", error))?;
    if wallet.get::<String, _>("status") != "active" {
        return Err(gql::BillingError::Unavailable);
    }
    let wallet_id = wallet.get::<i64, _>("id");
    let amount_micros = code_row.get::<i64, _>("amount_micros");
    let metadata = json!({
        "creditRedemptionBatchID": batch_id,
        "creditRedemptionCodeID": code_id,
    })
    .to_string();
    let ledger_entry_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO project_credit_ledger_entries \
         (wallet_id,amount_micros,entry_type,reference_type,reference_id,idempotency_key,description,metadata,created_at) \
         VALUES($1,$2,'redemption','credit_redemption_code',$3,$4,'Credit redemption code',$5,$6) \
         RETURNING id",
    )
    .bind(wallet_id)
    .bind(amount_micros)
    .bind(code_id.to_string())
    .bind(format!("credit-redemption:{code_id}:{user_id}"))
    .bind(metadata)
    .bind(redeemed_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("redeem_code.insert_ledger", error))?;
    let receipt_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO credit_redemption_receipts \
         (code_id,project_id,user_id,wallet_id,ledger_entry_id,amount_micros,currency,redeemed_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id",
    )
    .bind(code_id)
    .bind(project_id)
    .bind(user_id)
    .bind(wallet_id)
    .bind(ledger_entry_id)
    .bind(amount_micros)
    .bind(STATION_CREDIT_CODE)
    .bind(redeemed_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| sanitized_db_error("redeem_code.insert_receipt", error))?;
    if updated_redemption_count == max_redemptions {
        let updated = sqlx::query(
            "UPDATE credit_redemption_codes \
             SET status='redeemed',redeemed_at=$2 WHERE id=$1 AND status='active'",
        )
        .bind(code_id)
        .bind(redeemed_at)
        .execute(&mut *tx)
        .await
        .map_err(|error| sanitized_db_error("redeem_code.update_code", error))?;
        if updated.rows_affected() != 1 {
            return Err(internal_storage_error(
                "redeem_code.update_code",
                "final redemption did not transition the code",
            ));
        }
    }
    insert_audit(
        &mut tx,
        &actor,
        AuditRow {
            operation: "redeem_code",
            batch_id,
            code_id: Some(code_id),
            receipt_id: Some(receipt_id),
            project_id: Some(project_id),
            user_id: Some(user_id),
            outcome: "success",
            detail: json!({
                "walletID": wallet_id,
                "ledgerEntryID": ledger_entry_id,
                "redemptionCount": updated_redemption_count,
                "maxRedemptions": max_redemptions,
            }),
            created_at: redeemed_at,
        },
    )
    .await?;
    tx.commit()
        .await
        .map_err(|error| sanitized_db_error("redeem_code.commit", error))?;

    Ok(gql::CreditRedemptionReceipt {
        id: id(receipt_id),
        code_id: id(code_id),
        project_id: id(project_id),
        user_id: id(user_id),
        amount: amount(amount_micros),
        currency: STATION_CREDIT_CODE.to_string(),
        redeemed_at: wire_time(redeemed_at),
    })
}

fn ensure_actor_matches_user(
    actor: &gql::CreditRedemptionActor,
    user_id: i64,
) -> Result<(), gql::BillingError> {
    let actor_user_id = actor
        .actor_id
        .as_deref()
        .and_then(|value| parse_id(value, "actor user ID").ok());
    if actor.actor_type == "user" && actor_user_id == Some(user_id) {
        Ok(())
    } else {
        Err(gql::BillingError::Invalid(
            "authenticated actor does not match redemption user".to_string(),
        ))
    }
}

async fn ensure_active_membership_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    project_id: i64,
) -> Result<(), gql::BillingError> {
    let authorized = sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM user_projects up \
         JOIN users u ON u.id=up.user_id \
         JOIN projects p ON p.id=up.project_id \
         WHERE up.user_id=$1 AND up.project_id=$2 \
           AND u.status='activated' AND u.deleted_at=0 \
           AND p.status='active' AND p.deleted_at=0 \
         FOR SHARE OF up,u,p",
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| sanitized_db_error("redeem_code.authorize", error))?;
    if authorized.is_some() {
        Ok(())
    } else {
        Err(gql::BillingError::Invalid(
            "authenticated user is not an active member of the selected project".to_string(),
        ))
    }
}

struct AuditRow {
    operation: &'static str,
    batch_id: i64,
    code_id: Option<i64>,
    receipt_id: Option<i64>,
    project_id: Option<i64>,
    user_id: Option<i64>,
    outcome: &'static str,
    detail: Value,
    created_at: DateTime<Utc>,
}

async fn insert_audit(
    tx: &mut Transaction<'_, Postgres>,
    actor: &gql::CreditRedemptionActor,
    row: AuditRow,
) -> Result<(), gql::BillingError> {
    sqlx::query(
        "INSERT INTO credit_redemption_transaction_audits \
         (operation,actor_type,actor_id,batch_id,code_id,receipt_id,project_id,user_id,outcome,detail_snapshot,created_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(row.operation)
    .bind(&actor.actor_type)
    .bind(actor.actor_id.as_deref())
    .bind(row.batch_id)
    .bind(row.code_id)
    .bind(row.receipt_id)
    .bind(row.project_id)
    .bind(row.user_id)
    .bind(row.outcome)
    .bind(Json(row.detail))
    .bind(row.created_at)
    .execute(&mut **tx)
    .await
    .map_err(|error| sanitized_db_error("insert_audit", error))?;
    Ok(())
}

async fn transaction_time(
    tx: &mut Transaction<'_, Postgres>,
    operation: &'static str,
) -> Result<DateTime<Utc>, gql::BillingError> {
    sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| sanitized_db_error(operation, error))
}

async fn wall_clock_time(
    tx: &mut Transaction<'_, Postgres>,
    operation: &'static str,
) -> Result<DateTime<Utc>, gql::BillingError> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| sanitized_db_error(operation, error))
}

fn code_from_row(row: &PgRow) -> gql::CreditRedemptionCode {
    let expires_at = row.get::<Option<DateTime<Utc>>, _>("expires_at");
    let read_at = row.get::<DateTime<Utc>, _>("read_at");
    let max_redemptions = row.get::<i32, _>("max_redemptions");
    let redemption_count = row.get::<i32, _>("redemption_count");
    let status = match row.get::<String, _>("status").as_str() {
        "redeemed" => gql::CreditRedemptionCodeStatus::Redeemed,
        "revoked" => gql::CreditRedemptionCodeStatus::Revoked,
        "active" if expires_at.is_some_and(|expiry| expiry <= read_at) => {
            gql::CreditRedemptionCodeStatus::Expired
        }
        _ => gql::CreditRedemptionCodeStatus::Active,
    };
    gql::CreditRedemptionCode {
        id: id(row.get("id")),
        batch_id: id(row.get("batch_id")),
        code_hint: row.get("code_hint"),
        amount: amount(row.get("amount_micros")),
        currency: row.get("currency"),
        description: row.get("description"),
        max_redemptions,
        redemption_count,
        remaining_redemptions: max_redemptions.saturating_sub(redemption_count),
        status,
        expires_at: expires_at.map(wire_time),
        redeemed_at: row
            .get::<Option<DateTime<Utc>>, _>("redeemed_at")
            .map(wire_time),
        revoked_at: row
            .get::<Option<DateTime<Utc>>, _>("revoked_at")
            .map(wire_time),
        created_at: wire_time(row.get("created_at")),
    }
}

fn receipt_from_row(row: &PgRow) -> gql::CreditRedemptionReceipt {
    gql::CreditRedemptionReceipt {
        id: id(row.get("id")),
        code_id: id(row.get("code_id")),
        project_id: id(row.get("project_id")),
        user_id: id(row.get("user_id")),
        amount: amount(row.get("amount_micros")),
        currency: row.get("currency"),
        redeemed_at: wire_time(row.get("redeemed_at")),
    }
}

fn generate_code() -> Result<String, gql::BillingError> {
    generate_api_key(REDEMPTION_CODE_PREFIX).map_err(|_| {
        internal_storage_error("create_codes.generate", "CSPRNG code generation failed")
    })
}

fn digest_code(normalized_code: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(normalized_code.as_bytes()))
}

fn code_hint(normalized_code: &str) -> String {
    let suffix_start = normalized_code.len().saturating_sub(REDEMPTION_HINT_CHARS);
    format!("****-{}", &normalized_code[suffix_start..])
}

fn parse_exact_amount_micros(value: &str) -> Result<i64, gql::BillingError> {
    gql::validate_credit_redemption_amount(value)?;
    let value = value.trim();
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let whole = whole.parse::<i64>().map_err(|_| {
        gql::BillingError::Invalid("amount must fit the credit ledger range".to_string())
    })?;
    let mut fraction_micros = fraction.to_string();
    fraction_micros.extend(std::iter::repeat_n('0', 6 - fraction.len()));
    let fraction_micros = fraction_micros.parse::<i64>().map_err(|_| {
        gql::BillingError::Invalid("amount must fit the credit ledger range".to_string())
    })?;
    whole
        .checked_mul(1_000_000)
        .and_then(|micros| micros.checked_add(fraction_micros))
        .filter(|micros| *micros > 0)
        .ok_or_else(|| {
            gql::BillingError::Invalid("amount must fit the credit ledger range".to_string())
        })
}

fn validate_batch_liability(
    amount_micros: i64,
    quantity: i32,
    max_redemptions: i32,
) -> Result<(), gql::BillingError> {
    amount_micros
        .checked_mul(i64::from(quantity))
        .and_then(|total| total.checked_mul(i64::from(max_redemptions)))
        .ok_or_else(|| {
            gql::BillingError::Invalid(
                "total redemption value must fit the credit ledger range".to_string(),
            )
        })?;
    Ok(())
}

fn parse_expiry(value: &str) -> Result<DateTime<Utc>, gql::BillingError> {
    DateTime::parse_from_rfc3339(value.trim())
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| gql::BillingError::Invalid("expiresAt must be RFC 3339".to_string()))
}

fn parse_id(value: &str, field: &str) -> Result<i64, gql::BillingError> {
    value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| gql::BillingError::Invalid(format!("invalid {field}")))
}

fn id(value: i64) -> ID {
    ID(value.to_string())
}

fn amount(value: i64) -> String {
    micros_to_decimal(value).normalize().to_string()
}

fn wire_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn sanitized_db_error(operation: &'static str, error: sqlx::Error) -> gql::BillingError {
    tracing::error!(operation, error = %error, "credit redemption storage operation failed");
    gql::BillingError::Storage(STORAGE_ERROR.to_string())
}

fn internal_storage_error(operation: &'static str, reason: &'static str) -> gql::BillingError {
    tracing::error!(operation, reason, "credit redemption invariant failed");
    gql::BillingError::Storage(STORAGE_ERROR.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use sqlx::types::Json;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn generated_codes_are_high_entropy_prefixed_and_unique() -> TestResult {
        let first = generate_code()?;
        let second = generate_code()?;
        assert!(first.starts_with("conduit-credit-"));
        assert_eq!(first.len(), REDEMPTION_CODE_PREFIX.len() + 1 + 64);
        assert_ne!(first, second);
        Ok(())
    }

    #[test]
    fn digest_uses_normalized_code_without_retaining_plaintext() -> TestResult {
        let plaintext = generate_code()?;
        let normalized = gql::normalize_credit_redemption_code(&plaintext)?;
        let digest = digest_code(&normalized);
        assert_eq!(digest.len(), "sha256:".len() + 64);
        assert!(digest.starts_with("sha256:"));
        assert!(
            digest["sha256:".len()..]
                .bytes()
                .all(|byte| { byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) })
        );
        assert!(!digest.contains(&plaintext));
        assert_eq!(digest, digest_code(&plaintext.to_ascii_uppercase()));
        Ok(())
    }

    #[test]
    fn exact_amount_parser_rejects_rounding_and_preserves_six_decimals() -> TestResult {
        assert_eq!(parse_exact_amount_micros("1.000001")?, 1_000_001);
        assert_eq!(parse_exact_amount_micros(" 12.34 ")?, 12_340_000);
        assert!(parse_exact_amount_micros("1.0000001").is_err());
        assert!(parse_exact_amount_micros("0.0000001").is_err());
        Ok(())
    }

    #[test]
    fn batch_liability_multiplication_is_checked() -> TestResult {
        validate_batch_liability(1, 1_000, 100_000)?;
        validate_batch_liability(i64::MAX, 1, 1)?;
        assert!(validate_batch_liability(i64::MAX, 2, 1).is_err());
        assert!(validate_batch_liability(i64::MAX / 2 + 1, 1, 2).is_err());
        Ok(())
    }

    #[test]
    fn redemption_actor_must_be_the_target_user() {
        let matching = gql::CreditRedemptionActor {
            actor_type: "user".to_string(),
            actor_id: Some("42".to_string()),
        };
        assert!(ensure_actor_matches_user(&matching, 42).is_ok());
        assert!(ensure_actor_matches_user(&matching, 7).is_err());
        let api_key = gql::CreditRedemptionActor {
            actor_type: "api_key".to_string(),
            actor_id: Some("42".to_string()),
        };
        assert!(ensure_actor_matches_user(&api_key, 42).is_err());
    }

    #[tokio::test]
    async fn concurrent_distinct_users_cannot_oversubscribe_code_limit() -> TestResult {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let issuer_id = insert_user(&pool, &suffix, "limit-issuer").await?;
        let project_id = insert_project(&pool, &suffix, "limit-project").await?;
        insert_membership(&pool, issuer_id, project_id).await?;

        let created = create_codes(
            &pool,
            redemption_actor(issuer_id),
            gql::CreateCreditRedemptionCodesInput {
                amount: "2".to_string(),
                quantity: 1,
                max_redemptions: 3,
                expires_at: None,
                description: Some("concurrent limit test".to_string()),
            },
        )
        .await?;
        let plaintext = created.codes[0].code.clone();
        let code_id = parse_id(created.codes[0].id.as_str(), "code ID")?;

        let mut contestants = Vec::new();
        for index in 0..8 {
            let user_id = insert_user(&pool, &suffix, &format!("limit-{index}")).await?;
            insert_membership(&pool, user_id, project_id).await?;
            contestants.push(user_id);
        }

        let mut tasks = Vec::new();
        for user_id in contestants.iter().copied() {
            let task_pool = pool.clone();
            let task_code = plaintext.clone();
            tasks.push(tokio::spawn(async move {
                let result = redeem_code(
                    &task_pool,
                    redemption_actor(user_id),
                    &user_id.to_string(),
                    &project_id.to_string(),
                    &task_code,
                )
                .await;
                (user_id, result)
            }));
        }

        let mut winners = Vec::new();
        let mut rejected = Vec::new();
        for task in tasks {
            let (user_id, result) = task.await?;
            match result {
                Ok(receipt) => winners.push((user_id, receipt)),
                Err(gql::BillingError::RedemptionCodeUnavailable) => rejected.push(user_id),
                Err(error) => return Err(error.into()),
            }
        }
        assert_eq!(winners.len(), 3);
        assert_eq!(rejected.len(), 5);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM credit_redemption_receipts WHERE code_id=$1"
            )
            .bind(code_id)
            .fetch_one(&pool)
            .await?,
            3
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM project_credit_ledger_entries WHERE entry_type='redemption'"
            )
            .fetch_one(&pool)
            .await?,
            3
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT credit_balance_micros FROM project_wallets WHERE project_id=$1"
            )
            .bind(project_id)
            .fetch_one(&pool)
            .await?,
            6_000_000
        );

        let listed = list_codes(&pool, 50, 0).await?;
        let listed_code = listed
            .items
            .iter()
            .find(|code| code.id.as_str() == code_id.to_string())
            .ok_or("created redemption code missing from listing")?;
        assert_eq!(
            listed_code.status,
            gql::CreditRedemptionCodeStatus::Redeemed
        );
        assert_eq!(listed_code.max_redemptions, 3);
        assert_eq!(listed_code.redemption_count, 3);
        assert_eq!(listed_code.remaining_redemptions, 0);

        let (winner_id, winner_receipt) = &winners[0];
        let replay = redeem_code(
            &pool,
            redemption_actor(*winner_id),
            &winner_id.to_string(),
            &project_id.to_string(),
            &plaintext,
        )
        .await?;
        assert_eq!(replay.id, winner_receipt.id);

        let rejected_id = rejected[0];
        assert!(matches!(
            redeem_code(
                &pool,
                redemption_actor(rejected_id),
                &rejected_id.to_string(),
                &project_id.to_string(),
                &plaintext,
            )
            .await,
            Err(gql::BillingError::RedemptionCodeUnavailable)
        ));

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn redemption_is_atomic_idempotent_and_never_persists_plaintext() -> TestResult {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let user_id = insert_user(&pool, &suffix, "owner").await?;
        let project_id = insert_project(&pool, &suffix, "primary").await?;
        insert_membership(&pool, user_id, project_id).await?;
        let actor = gql::CreditRedemptionActor {
            actor_type: "user".to_string(),
            actor_id: Some(user_id.to_string()),
        };
        let created = create_codes(
            &pool,
            actor.clone(),
            gql::CreateCreditRedemptionCodesInput {
                amount: "12.345678".to_string(),
                quantity: 2,
                max_redemptions: 2,
                expires_at: Some("2999-01-01T00:00:00Z".to_string()),
                description: Some("integration campaign".to_string()),
            },
        )
        .await?;
        assert_eq!(created.codes.len(), 2);
        assert_eq!(created.max_redemptions, 2);
        let plaintext = created.codes[0].code.clone();
        let normalized = gql::normalize_credit_redemption_code(&plaintext)?;

        let mut tasks = Vec::new();
        for _ in 0..24 {
            let task_pool = pool.clone();
            let task_actor = actor.clone();
            let task_code = plaintext.clone();
            tasks.push(tokio::spawn(async move {
                redeem_code(
                    &task_pool,
                    task_actor,
                    &user_id.to_string(),
                    &project_id.to_string(),
                    &task_code,
                )
                .await
            }));
        }
        let mut receipt_ids = BTreeSet::new();
        for task in tasks {
            receipt_ids.insert(task.await??.id.to_string());
        }
        assert_eq!(receipt_ids.len(), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM credit_redemption_receipts")
                .fetch_one(&pool)
                .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM project_credit_ledger_entries WHERE entry_type='redemption'"
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT credit_balance_micros FROM project_wallets WHERE project_id=$1"
            )
            .bind(project_id)
            .fetch_one(&pool)
            .await?,
            12_345_678
        );
        let outcomes = sqlx::query(
            "SELECT outcome,COUNT(*)::BIGINT AS count \
             FROM credit_redemption_transaction_audits \
             WHERE operation='redeem_code' GROUP BY outcome",
        )
        .fetch_all(&pool)
        .await?;
        let success = outcomes
            .iter()
            .find(|row| row.get::<String, _>("outcome") == "success")
            .map(|row| row.get::<i64, _>("count"));
        let replayed = outcomes
            .iter()
            .find(|row| row.get::<String, _>("outcome") == "replayed")
            .map(|row| row.get::<i64, _>("count"));
        assert_eq!(success, Some(1));
        assert_eq!(replayed, Some(23));

        let other_user = insert_user(&pool, &suffix, "other").await?;
        insert_membership(&pool, other_user, project_id).await?;
        let other_user_result = redeem_code(
            &pool,
            redemption_actor(other_user),
            &other_user.to_string(),
            &project_id.to_string(),
            &plaintext,
        )
        .await?;
        assert_ne!(
            other_user_result.id.to_string(),
            receipt_ids.first().cloned().unwrap_or_default()
        );

        let original_replay = redeem_code(
            &pool,
            actor.clone(),
            &user_id.to_string(),
            &project_id.to_string(),
            &plaintext,
        )
        .await?;
        assert!(receipt_ids.contains(original_replay.id.as_str()));

        let third_user = insert_user(&pool, &suffix, "third").await?;
        insert_membership(&pool, third_user, project_id).await?;
        let exhausted_result = redeem_code(
            &pool,
            redemption_actor(third_user),
            &third_user.to_string(),
            &project_id.to_string(),
            &plaintext,
        )
        .await;
        assert!(matches!(
            exhausted_result,
            Err(gql::BillingError::RedemptionCodeUnavailable)
        ));
        let other_project = insert_project(&pool, &suffix, "other").await?;
        insert_membership(&pool, user_id, other_project).await?;
        let other_project_result = redeem_code(
            &pool,
            actor.clone(),
            &user_id.to_string(),
            &other_project.to_string(),
            &plaintext,
        )
        .await;
        assert!(matches!(
            other_project_result,
            Err(gql::BillingError::RedemptionCodeUnavailable)
        ));

        // Authorization is evaluated before code state and locks the user,
        // Project, and membership rows. All invalid principals receive the
        // same membership error without learning whether this second code is
        // active.
        let second_plaintext = created.codes[1].code.clone();
        let nonmember = insert_user(&pool, &suffix, "nonmember").await?;
        let nonmember_result = redeem_code(
            &pool,
            redemption_actor(nonmember),
            &nonmember.to_string(),
            &project_id.to_string(),
            &second_plaintext,
        )
        .await;
        assert!(matches!(
            nonmember_result,
            Err(gql::BillingError::Invalid(_))
        ));
        sqlx::query("UPDATE users SET status='deactivated' WHERE id=$1")
            .bind(other_user)
            .execute(&pool)
            .await?;
        let inactive_user_result = redeem_code(
            &pool,
            redemption_actor(other_user),
            &other_user.to_string(),
            &project_id.to_string(),
            &second_plaintext,
        )
        .await;
        assert!(matches!(
            inactive_user_result,
            Err(gql::BillingError::Invalid(_))
        ));
        sqlx::query("UPDATE projects SET status='archived' WHERE id=$1")
            .bind(other_project)
            .execute(&pool)
            .await?;
        let archived_project_result = redeem_code(
            &pool,
            actor.clone(),
            &user_id.to_string(),
            &other_project.to_string(),
            &second_plaintext,
        )
        .await;
        assert!(matches!(
            archived_project_result,
            Err(gql::BillingError::Invalid(_))
        ));

        let persisted_code =
            sqlx::query("SELECT code_digest,code_hint FROM credit_redemption_codes WHERE id=$1")
                .bind(parse_id(created.codes[0].id.as_str(), "code ID")?)
                .fetch_one(&pool)
                .await?;
        let digest = persisted_code.get::<String, _>("code_digest");
        let hint = persisted_code.get::<String, _>("code_hint");
        assert_ne!(digest, normalized);
        assert!(!digest.contains(&normalized));
        assert_ne!(hint, normalized);
        let persisted_text = sqlx::query_scalar::<_, String>(
            "SELECT COALESCE(string_agg(value,' '),'') FROM ( \
               SELECT metadata AS value FROM project_credit_ledger_entries \
               UNION ALL SELECT COALESCE(description,'') FROM project_credit_ledger_entries \
               UNION ALL SELECT detail_snapshot::text FROM credit_redemption_transaction_audits \
             ) persisted",
        )
        .fetch_one(&pool)
        .await?;
        assert!(!persisted_text.contains(&normalized));
        assert!(!persisted_text.contains(&plaintext));

        let partial_receipt = redeem_code(
            &pool,
            actor.clone(),
            &user_id.to_string(),
            &project_id.to_string(),
            &second_plaintext,
        )
        .await?;
        let revoked = revoke_code(&pool, actor.clone(), created.codes[1].id.as_str()).await?;
        assert_eq!(revoked.status, gql::CreditRedemptionCodeStatus::Revoked);
        assert_eq!(revoked.max_redemptions, 2);
        assert_eq!(revoked.redemption_count, 1);
        assert_eq!(revoked.remaining_redemptions, 1);
        assert_eq!(revoked.description.as_deref(), Some("integration campaign"));
        let revoked_replay = redeem_code(
            &pool,
            actor.clone(),
            &user_id.to_string(),
            &project_id.to_string(),
            &second_plaintext,
        )
        .await?;
        assert_eq!(revoked_replay.id, partial_receipt.id);
        let revoked_result = redeem_code(
            &pool,
            redemption_actor(third_user),
            &third_user.to_string(),
            &project_id.to_string(),
            &second_plaintext,
        )
        .await;
        assert!(matches!(
            revoked_result,
            Err(gql::BillingError::RedemptionCodeUnavailable)
        ));
        assert!(
            revoke_code(&pool, actor.clone(), created.codes[1].id.as_str())
                .await
                .is_err()
        );
        let terminal_transition = sqlx::query(
            "UPDATE credit_redemption_codes SET status='active',revoked_at=NULL WHERE id=$1",
        )
        .bind(parse_id(created.codes[1].id.as_str(), "code ID")?)
        .execute(&pool)
        .await;
        assert!(terminal_transition.is_err());

        let expired_plaintext = generate_code()?;
        let expired_normalized = gql::normalize_credit_redemption_code(&expired_plaintext)?;
        let expired_batch_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO credit_redemption_batches \
             (amount_micros,currency,quantity,expires_at,created_by_actor_type,created_by_actor_id,created_at) \
             VALUES(1000000,$1,1,$2,'user',$3,$4) RETURNING id",
        )
        .bind(STATION_CREDIT_CODE)
        .bind(Utc::now() - chrono::Duration::seconds(1))
        .bind(user_id.to_string())
        .bind(Utc::now())
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO credit_redemption_codes \
             (batch_id,code_digest,code_hint,status,created_at) \
             VALUES($1,$2,$3,'active',$4)",
        )
        .bind(expired_batch_id)
        .bind(digest_code(&expired_normalized))
        .bind(code_hint(&expired_normalized))
        .bind(Utc::now())
        .execute(&pool)
        .await?;
        let expired_result = redeem_code(
            &pool,
            actor,
            &user_id.to_string(),
            &project_id.to_string(),
            &expired_plaintext,
        )
        .await;
        assert!(matches!(
            expired_result,
            Err(gql::BillingError::RedemptionCodeUnavailable)
        ));
        database.cleanup().await?;
        Ok(())
    }

    async fn insert_user(pool: &PgPool, suffix: &str, label: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO users(email,status,prefer_language,password,first_name,last_name,is_owner,scopes) \
             VALUES($1,'activated','en','test','Redemption','Test',FALSE,$2) RETURNING id",
        )
        .bind(format!("redemption-{label}-{suffix}@example.test"))
        .bind(Json(Vec::<String>::new()))
        .fetch_one(pool)
        .await
    }

    fn redemption_actor(user_id: i64) -> gql::CreditRedemptionActor {
        gql::CreditRedemptionActor {
            actor_type: "user".to_string(),
            actor_id: Some(user_id.to_string()),
        }
    }

    async fn insert_project(pool: &PgPool, suffix: &str, label: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO projects(name,description,status,profiles) \
             VALUES($1,'redemption integration','active','{}'::jsonb) RETURNING id",
        )
        .bind(format!("Redemption {label} {suffix}"))
        .fetch_one(pool)
        .await
    }

    async fn insert_membership(
        pool: &PgPool,
        user_id: i64,
        project_id: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO user_projects(user_id,project_id,is_owner,scopes) \
             VALUES($1,$2,FALSE,$3)",
        )
        .bind(user_id)
        .bind(project_id)
        .bind(Json(Vec::<String>::new()))
        .execute(pool)
        .await?;
        Ok(())
    }
}
