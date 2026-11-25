use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use conduit_admin_graphql::change_package as package;
use conduit_admin_graphql::{
    pagination::PageInfo,
    scalars::{CursorScalar, TimeScalar},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;

fn conflict(code: &str, message: impl std::fmt::Display) -> gql::ChangeSetError {
    gql::ChangeSetError::Invalid(format!("{code}: {message}"))
}
fn digest(value: &Value) -> String {
    // PostgreSQL JSONB does not preserve insertion order. All operation/base
    // fingerprints must survive a write/read cycle without treating key order
    // as a new user intent or a changed upstream observation.
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), canonical(value)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    format!(
        "{:x}",
        Sha256::digest(canonical(value).to_string().as_bytes())
    )
}
fn operation(id: &ID) -> Result<Uuid, gql::ChangeSetError> {
    Uuid::parse_str(id.as_str())
        .map_err(|_| conflict("OPERATION_CONFLICT", "operationID must be a UUID"))
}
fn count(n: i64) -> Result<i32, gql::ChangeSetError> {
    i32::try_from(n).map_err(|_| gql::ChangeSetError::Query("count exceeds GraphQL Int".into()))
}
fn issue(key: Option<String>, message: String) -> package::ChangeSetIssue {
    package::ChangeSetIssue {
        item_key: key,
        field: None,
        code: if message.starts_with("SOURCE_CHANGED:") {
            "SOURCE_CHANGED"
        } else {
            "VALIDATION_FAILED"
        }
        .into(),
        message,
        severity: package::IssueSeverity::Blocking,
    }
}
fn page_size(first: i32) -> Result<i64, gql::ChangeSetError> {
    if (1..=100).contains(&first) {
        Ok(i64::from(first))
    } else {
        Err(conflict("BAD_CURSOR", "first must be between 1 and 100"))
    }
}
fn cursor(scope: &str, value: Value) -> CursorScalar {
    CursorScalar(URL_SAFE_NO_PAD.encode(json!({"scope":scope,"value":value}).to_string()))
}
fn decode_cursor(
    after: Option<CursorScalar>,
    scope: &str,
) -> Result<Option<Value>, gql::ChangeSetError> {
    after
        .map(|c| {
            let bytes = URL_SAFE_NO_PAD
                .decode(c.0)
                .map_err(|_| conflict("BAD_CURSOR", "invalid cursor"))?;
            let v: Value = serde_json::from_slice(&bytes)
                .map_err(|_| conflict("BAD_CURSOR", "invalid cursor"))?;
            if v.get("scope").and_then(Value::as_str) != Some(scope) {
                return Err(conflict("BAD_CURSOR", "cursor belongs to another filter"));
            }
            Ok(v["value"].clone())
        })
        .transpose()
}
fn page_info(cursors: &[CursorScalar], next: bool, previous: bool) -> PageInfo {
    PageInfo {
        has_next_page: next,
        has_previous_page: previous,
        start_cursor: cursors.first().cloned(),
        end_cursor: cursors.last().cloned(),
    }
}
async fn revision(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
) -> Result<String, gql::ChangeSetError> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(id),0) FROM change_set_events WHERE change_set_id=$1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await
    .map(|v| v.to_string())
    .map_err(storage_error)
}
pub(crate) async fn current_revision(pool: &PgPool, id: ID) -> Result<String, gql::ChangeSetError> {
    let mut tx = pool.begin().await.map_err(storage_error)?;
    let v = revision(&mut tx, parse_numeric_id(id.as_str(), "package")?).await?;
    tx.commit().await.map_err(storage_error)?;
    Ok(v)
}
async fn expect_revision(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    expected: &str,
) -> Result<(), gql::ChangeSetError> {
    if revision(tx, id).await? != expected {
        return Err(conflict(
            "REVISION_CONFLICT",
            "package changed; reload its latest revision",
        ));
    }
    Ok(())
}
/// Every lifecycle writer takes the immutable scope lock BEFORE the parent lock.
async fn lock_package(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
) -> Result<ChangeSetRow, gql::ChangeSetError> {
    let scope = sqlx::query_as::<_, (String, String, String)>(
        "SELECT kind,scope_type,scope_id FROM change_sets WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage_error)?
    .ok_or_else(|| conflict("STATE_CHANGED", "package not found"))?;
    lock_scope(tx, &scope.0, &scope.1, &scope.2).await?;
    let row = lock_change_set(tx, id).await?;
    if (
        row.kind.as_str(),
        row.scope_type.as_str(),
        row.scope_id.as_str(),
    ) != (scope.0.as_str(), scope.1.as_str(), scope.2.as_str())
    {
        return Err(conflict("STATE_CHANGED", "package scope changed"));
    }
    Ok(row)
}
async fn lock_scope(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    scope_type: &str,
    id: &str,
) -> Result<(), gql::ChangeSetError> {
    // Refresh and publication acquire the price-input lock before scope and
    // package rows. A formula can retire its draft without forming a reverse
    // lock order with manual edits or publication.
    crate::wiring::lock_accounting_currency_price_writes(tx)
        .await
        .map_err(storage_error)?;
    if scope_type == "channel" {
        crate::wiring_postgres_provider_pricing::lock_provider_pricing_channel(
            tx,
            parse_numeric_id(id, "channel")?,
        )
        .await
        .map_err(gql::ChangeSetError::Storage)?;
    } else {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("change-package:{kind}:{scope_type}:{id}"))
            .execute(&mut **tx)
            .await
            .map_err(storage_error)?;
    }
    Ok(())
}
async fn lock_operation(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<(), gql::ChangeSetError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("change-operation:{id}"))
        .execute(&mut **tx)
        .await
        .map_err(storage_error)?;
    Ok(())
}

async fn summaries(
    pool: &PgPool,
    ids: &[i64],
) -> Result<BTreeMap<i64, package::ChangeSetSummary>, gql::ChangeSetError> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    // Snapshot bodies remain in PostgreSQL. Aggregate only this selected page.
    let rows=sqlx::query("SELECT cs.*,counts.item_count,counts.create_count,counts.update_count,counts.delete_count,counts.invalid_count,events.revision,events.origin,COALESCE(c.name,b.name) AS scope_name, NULLIF(trim(concat_ws(' ',u.first_name,u.last_name)),'') AS actor_name FROM change_sets cs LEFT JOIN channels c ON cs.scope_type='channel' AND cs.scope_id=c.id::text LEFT JOIN price_books b ON cs.scope_type='price_book' AND cs.scope_id=b.id::text LEFT JOIN users u ON u.id=cs.created_by LEFT JOIN (SELECT change_set_id,COUNT(*) AS item_count,COUNT(*) FILTER(WHERE action='create') AS create_count,COUNT(*) FILTER(WHERE action='update') AS update_count,COUNT(*) FILTER(WHERE action='delete') AS delete_count,COUNT(*) FILTER(WHERE validation_error IS NOT NULL OR (action='delete' AND (before_snapshot IS NULL OR after_snapshot IS NOT NULL)) OR (action<>'delete' AND after_snapshot IS NULL)) AS invalid_count FROM change_set_items WHERE change_set_id=ANY($1) GROUP BY change_set_id) counts ON counts.change_set_id=cs.id LEFT JOIN (SELECT change_set_id,MAX(id) AS revision,(array_agg(detail->>'origin' ORDER BY id) FILTER(WHERE detail ? 'origin'))[1] AS origin FROM change_set_events WHERE change_set_id=ANY($1) GROUP BY change_set_id) events ON events.change_set_id=cs.id WHERE cs.id=ANY($1)").bind(ids).fetch_all(pool).await.map_err(query_error)?;
    rows.into_iter()
        .map(|r| {
            let id: i64 = r.get("id");
            let items = count(r.get::<Option<i64>, _>("item_count").unwrap_or(0))?;
            let invalid = count(r.get::<Option<i64>, _>("invalid_count").unwrap_or(0))?;
            let state = parse_status(r.get::<String, _>("status").as_str())?;
            let validation = if invalid > 0 || state == gql::ChangeSetStatus::Invalid {
                package::ValidationState::Invalid
            } else if items == 0 {
                package::ValidationState::Unchecked
            } else {
                package::ValidationState::Valid
            };
            Ok((
                id,
                package::ChangeSetSummary {
                    id: ID(id.to_string()),
                    operation_id: r
                        .get::<Option<Uuid>, _>("operation_id")
                        .map(|o| ID(o.to_string())),
                    kind: parse_kind(&r.get::<String, _>("kind"))?,
                    scope_type: r.get("scope_type"),
                    scope_id: r.get("scope_id"),
                    scope_name: r.get("scope_name"),
                    title: r.get("title"),
                    status: state,
                    revision: r.get::<Option<i64>, _>("revision").unwrap_or(0).to_string(),
                    source_origin: package::OperationOrigin::from_stored(
                        r.get::<Option<String>, _>("origin")
                            .as_deref()
                            .unwrap_or("UNKNOWN"),
                    ),
                    created_by: r
                        .get::<Option<i64>, _>("created_by")
                        .map(|n| ID(n.to_string())),
                    actor_name: r.get("actor_name"),
                    created_at: TimeScalar(r.get::<DateTime<Utc>, _>("created_at")),
                    updated_at: TimeScalar(r.get::<DateTime<Utc>, _>("updated_at")),
                    submitted_at: r
                        .get::<Option<DateTime<Utc>>, _>("submitted_at")
                        .map(TimeScalar),
                    reviewed_at: r
                        .get::<Option<DateTime<Utc>>, _>("reviewed_at")
                        .map(TimeScalar),
                    applied_at: r
                        .get::<Option<DateTime<Utc>>, _>("applied_at")
                        .map(TimeScalar),
                    item_count: items,
                    create_count: count(r.get::<Option<i64>, _>("create_count").unwrap_or(0))?,
                    update_count: count(r.get::<Option<i64>, _>("update_count").unwrap_or(0))?,
                    delete_count: count(r.get::<Option<i64>, _>("delete_count").unwrap_or(0))?,
                    invalid_item_count: invalid,
                    validation_state: validation,
                    validation_error: r.get("validation_error"),
                },
            ))
        })
        .collect()
}
async fn committed_provider_detail(
    pool: &PgPool,
    viewer: package::PackageViewer,
    id: i64,
) -> Result<package::ChangeSetDetail, gql::ChangeSetError> {
    detail(pool, viewer, id)
        .await
        .map_err(|error| match error {
            // A package and its receipt already exist when loading this response.
            gql::ChangeSetError::Invalid(message) | gql::ChangeSetError::Query(message) => {
                gql::ChangeSetError::Storage(message)
            }
            other => other,
        })?
        .ok_or_else(|| {
            gql::ChangeSetError::Storage("committed provider price package is missing".into())
        })
}

async fn detail(
    pool: &PgPool,
    viewer: package::PackageViewer,
    id: i64,
) -> Result<Option<package::ChangeSetDetail>, gql::ChangeSetError> {
    let Some(summary) = summaries(pool, &[id]).await?.remove(&id) else {
        return Ok(None);
    };
    let row = sqlx::query_as::<_, ChangeSetRow>(sqlx::AssertSqlSafe::<&str>(&format!(
        "SELECT {CHANGE_SET_COLUMNS} FROM change_sets WHERE id=$1"
    )))
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(query_error)?;
    let issues=sqlx::query_as::<_,(String,String)>("SELECT item_key,validation_error FROM change_set_items WHERE change_set_id=$1 AND validation_error IS NOT NULL ORDER BY id").bind(id).fetch_all(pool).await.map_err(query_error)?.into_iter().map(|(key,msg)|issue(Some(key),msg)).collect();
    let mut actions = Vec::new();
    if viewer.can_write {
        use package::ChangeSetAvailableAction::*;
        match summary.status {
            gql::ChangeSetStatus::Draft => {
                actions.push(Edit);
                if summary.item_count > 0
                    && summary.validation_state == package::ValidationState::Valid
                {
                    actions.push(Submit);
                }
            }
            gql::ChangeSetStatus::PendingReview => {
                if summary.validation_state == package::ValidationState::Valid
                    && summary.item_count > 0
                {
                    actions.push(Approve);
                }
                actions.push(Reject);
            }
            gql::ChangeSetStatus::Approved => actions.push(Publish),
            gql::ChangeSetStatus::Invalid => {
                actions.push(Reject);
                actions.push(Recreate);
            }
            gql::ChangeSetStatus::Superseded => actions.push(Recreate),
            _ => {}
        }
    }
    if !actions.is_empty() {
        let mut tx = pool.begin().await.map_err(storage_error)?;
        let mut p = crate::wiring_governance::lock_policy(&mut tx)
            .await
            .map_err(gql::ChangeSetError::Invalid)?;
        let requires_review = if row.scope_type == "governed" {
            crate::wiring_governance_commands::requires_review(&mut tx, id, &p)
                .await
                .map_err(gql::ChangeSetError::Invalid)?
        } else {
            p.requires(&row.kind) || (row.kind == "model_mapping" && p.requires("model_supply"))
        };
        p.enabled = requires_review;
        p.review_kinds = if requires_review {
            vec![row.kind.clone()]
        } else {
            vec![]
        };
        if summary.status == gql::ChangeSetStatus::Draft
            && !requires_review
            && summary.validation_state == package::ValidationState::Valid
        {
            actions.retain(|a| *a != package::ChangeSetAvailableAction::Submit);
            actions.push(package::ChangeSetAvailableAction::Publish);
        }
        let domain_ok = if row.scope_type == "governed" {
            p.legacy_review_scopes = false;
            crate::wiring_governance_commands::authorize_package(&mut tx, id, viewer.actor_user_id)
                .await
                .is_ok()
        } else {
            crate::wiring_governance::authorize(
                &mut tx,
                viewer.actor_user_id,
                crate::wiring_governance::domain(&row.kind),
            )
            .await
            .is_ok()
        };
        if !domain_ok {
            actions.clear();
        } else {
            let mut allowed = Vec::new();
            for action in actions {
                let duty = match action {
                    package::ChangeSetAvailableAction::Approve
                    | package::ChangeSetAvailableAction::Reject => Some(false),
                    package::ChangeSetAvailableAction::Publish
                    | package::ChangeSetAvailableAction::ApproveApply => Some(true),
                    _ => None,
                };
                if let Some(publish) = duty {
                    if crate::wiring_governance::review_permission(
                        &mut tx,
                        &p,
                        viewer.actor_user_id,
                        row.created_by,
                        &row.kind,
                        publish,
                    )
                    .await
                    .is_err()
                    {
                        continue;
                    }
                }
                allowed.push(action);
            }
            actions = allowed;
        }
        tx.commit().await.map_err(storage_error)?;
    }
    let targets=sqlx::query_scalar::<_,SqlJson<Value>>("SELECT detail->'appliedTargets' FROM change_set_events WHERE change_set_id=$1 AND event_type='applied' AND detail ? 'appliedTargets' ORDER BY id DESC LIMIT 1").bind(id).fetch_optional(pool).await.map_err(query_error)?.map(|v|v.0).unwrap_or(json!([]));
    let applied_targets = targets
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            Some(package::ChangeSetAppliedTarget {
                r#type: v.get("type")?.as_str()?.to_string(),
                id: ID(v.get("id")?.as_str()?.to_string()),
                item_key: v.get("itemKey")?.as_str()?.to_string(),
            })
        })
        .collect();
    let current_base_revision = if row.scope_type == "governed" {
        None
    } else {
        match row.kind.as_str() {
            "provider_price" => Some(
                provider_base(pool, parse_numeric_id(&row.scope_id, "channel")?)
                    .await?
                    .revision,
            ),
            "retail_price" => Some(
                retail_base(pool, parse_numeric_id(&row.scope_id, "price book")?)
                    .await?
                    .revision,
            ),
            _ => None,
        }
    };
    Ok(Some(package::ChangeSetDetail {
        summary,
        base_revision: row.base_revision,
        source_revision: row.source_revision,
        current_base_revision,
        submitted_by: row.submitted_by.map(|n| ID(n.to_string())),
        reviewed_by: row.reviewed_by.map(|n| ID(n.to_string())),
        review_note: row.review_note,
        issues,
        available_actions: actions,
        applied_targets,
    }))
}

pub(crate) async fn provider_base_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
) -> Result<package::ProviderPriceEditBase, gql::ChangeSetError> {
    let settings=crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(tx).await.map_err(|e|gql::ChangeSetError::Storage(e.to_string()))?;
    let mut rows = conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(tx, id)
        .await
        .map_err(storage_error)?;
    rows.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    let revision = digest(
        &json!({"currency":settings.currency,"settingsVersion":settings.version,"prices":rows.iter().map(base_price_snapshot).collect::<Vec<_>>() }),
    );
    let prices = rows
        .into_iter()
        .map(crate::wiring::price_row_to_gql)
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage_error)?;
    Ok(package::ProviderPriceEditBase {
        channel_id: ID(id.to_string()),
        revision,
        accounting_currency: settings.currency,
        accounting_settings_version: settings.version.to_string(),
        prices,
    })
}
async fn provider_base(
    pool: &PgPool,
    id: i64,
) -> Result<package::ProviderPriceEditBase, gql::ChangeSetError> {
    let mut tx = pool.begin().await.map_err(storage_error)?;
    let v = provider_base_in_tx(&mut tx, id).await?;
    tx.commit().await.map_err(storage_error)?;
    Ok(v)
}
async fn retail_base_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
) -> Result<package::RetailPriceEditBase, gql::ChangeSetError> {
    let settings=crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(tx).await.map_err(|e|gql::ChangeSetError::Storage(e.to_string()))?;
    let version=sqlx::query_as::<_,(i64,String)>("SELECT id,reference_id FROM price_book_versions WHERE price_book_id=$1 AND status='published' ORDER BY version DESC LIMIT 1").bind(id).fetch_optional(&mut **tx).await.map_err(storage_error)?;
    let rows=match &version {Some((v,_))=>sqlx::query_as::<_,(i64,SqlJson<Value>)>("SELECT public_model_id,price FROM price_book_items WHERE price_book_version_id=$1 ORDER BY public_model_id").bind(v).fetch_all(&mut **tx).await.map_err(storage_error)?,None=>vec![]};
    let revision = digest(
        &json!({"reference":version.as_ref().map(|(_,r)|r),"currency":settings.currency,"settingsVersion":settings.version,"prices":rows.iter().map(|(id,p)|json!([id,p.0])).collect::<Vec<_>>() }),
    );
    Ok(package::RetailPriceEditBase {
        price_book_id: ID(id.to_string()),
        revision,
        accounting_currency: settings.currency,
        accounting_settings_version: settings.version.to_string(),
        prices: rows
            .into_iter()
            .map(|(n, p)| package::RetailPriceEditBaseItem {
                public_model_id: ID(n.to_string()),
                price: async_graphql::Json(p.0),
            })
            .collect(),
    })
}
async fn retail_base(
    pool: &PgPool,
    id: i64,
) -> Result<package::RetailPriceEditBase, gql::ChangeSetError> {
    let mut tx = pool.begin().await.map_err(storage_error)?;
    let v = retail_base_in_tx(&mut tx, id).await?;
    tx.commit().await.map_err(storage_error)?;
    Ok(v)
}

#[async_trait]
impl package::ChangePackageServices for PgChangeSetAdapter {
    async fn list_summaries(
        &self,
        filter: package::ChangeSetFilterInput,
        after: Option<CursorScalar>,
        first: i32,
    ) -> Result<package::ChangeSetSummaryConnection, gql::ChangeSetError> {
        let limit = page_size(first)?;
        let kind = filter.kind.map(gql::ChangeSetKind::as_str);
        let allowed = if let Some(v) = filter.allowed_kinds {
            v.into_iter()
                .map(|k| k.as_str().to_string())
                .collect::<Vec<_>>()
        } else {
            crate::wiring_governance_commands::capabilities()
                .map_err(gql::ChangeSetError::Storage)?
                .into_iter()
                .map(|c| c.kind.as_str().to_string())
                .chain([
                    "provider_price".into(),
                    "model_mapping".into(),
                    "retail_price".into(),
                ])
                .collect()
        };
        let statuses = filter
            .statuses
            .map(|v| {
                v.into_iter()
                    .map(|s| s.as_str().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec!["pending_review".into()]);
        let scope_type = normalize_filter(filter.scope_type);
        let scope_id = filter
            .scope_id
            .map(|v| parse_numeric_id(v.as_str(), "scope").map(|n| n.to_string()))
            .transpose()?;
        let search = normalize_filter(filter.search);
        let actor = filter.actor_user_id;
        let binding = digest(&json!([
            kind, statuses, scope_type, scope_id, search, allowed, actor
        ]));
        let decoded = decode_cursor(after.clone(), &binding)?;
        let time = decoded
            .as_ref()
            .map(|v| {
                v.get("time")
                    .and_then(Value::as_str)
                    .ok_or_else(|| conflict("BAD_CURSOR", "cursor timestamp missing"))
            })
            .transpose()?
            .map(|s| {
                DateTime::parse_from_rfc3339(s)
                    .map(|d| d.with_timezone(&Utc))
                    .map_err(|_| conflict("BAD_CURSOR", "invalid cursor timestamp"))
            })
            .transpose()?;
        let previous_id = decoded
            .as_ref()
            .map(|v| {
                v.get("id")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| conflict("BAD_CURSOR", "cursor id missing"))
            })
            .transpose()?;
        let predicate = "($1::text IS NULL OR kind=$1) AND ($2::text IS NULL OR scope_type=$2) AND ($3::text IS NULL OR scope_id=$3) AND ($4::text IS NULL OR title ILIKE '%'||$4||'%' OR scope_id=$4 OR operation_id::text=$4) AND kind=ANY($5) AND ($6::bigint IS NULL OR conduit_can_read_change_set($6,id))";
        let counts = sqlx::query_as::<_, (String, i64)>(sqlx::AssertSqlSafe::<&str>(&format!(
            "SELECT status,COUNT(*) FROM change_sets WHERE {predicate} GROUP BY status"
        )))
        .bind(kind)
        .bind(&scope_type)
        .bind(&scope_id)
        .bind(&search)
        .bind(&allowed)
        .bind(actor)
        .fetch_all(&self.pool)
        .await
        .map_err(query_error)?;
        let total_count = count(
            counts
                .iter()
                .filter(|(s, _)| statuses.is_empty() || statuses.contains(s))
                .map(|(_, n)| *n)
                .sum(),
        )?;
        let rows=sqlx::query_as::<_,(i64,DateTime<Utc>)>(sqlx::AssertSqlSafe::<&str>(&format!("SELECT id,updated_at FROM change_sets WHERE {predicate} AND (cardinality($7::text[])=0 OR status=ANY($7)) AND ($8::timestamptz IS NULL OR (updated_at,id)<($8,$9)) ORDER BY updated_at DESC,id DESC LIMIT $10"))).bind(kind).bind(scope_type).bind(scope_id).bind(search).bind(&allowed).bind(actor).bind(&statuses).bind(time).bind(previous_id).bind(limit+1).fetch_all(&self.pool).await.map_err(query_error)?;
        let next = rows.len() > limit as usize;
        let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
        let ids = rows.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        let mut meta = summaries(&self.pool, &ids).await?;
        let edges = rows
            .into_iter()
            .map(|(id, time)| {
                let node = meta.remove(&id).ok_or_else(|| {
                    gql::ChangeSetError::Query("package changed while listing; retry".into())
                })?;
                Ok(package::ChangeSetSummaryEdge {
                    cursor: cursor(&binding, json!({"id":id,"time":time.to_rfc3339()})),
                    node,
                })
            })
            .collect::<Result<Vec<_>, gql::ChangeSetError>>()?;
        let cursors = edges.iter().map(|e| e.cursor.clone()).collect::<Vec<_>>();
        Ok(package::ChangeSetSummaryConnection {
            edges,
            page_info: page_info(&cursors, next, after.is_some()),
            total_count,
            status_counts: counts
                .into_iter()
                .map(|(s, n)| {
                    Ok(package::ChangeSetStatusCount {
                        status: parse_status(&s)?,
                        count: count(n)?,
                    })
                })
                .collect::<Result<_, gql::ChangeSetError>>()?,
        })
    }
    async fn get_detail(
        &self,
        viewer: package::PackageViewer,
        id: ID,
    ) -> Result<Option<package::ChangeSetDetail>, gql::ChangeSetError> {
        detail(
            &self.pool,
            viewer,
            parse_numeric_id(id.as_str(), "change set")?,
        )
        .await
    }
    async fn list_items(
        &self,
        id: ID,
        after: Option<CursorScalar>,
        first: i32,
        search: Option<String>,
        problems_only: bool,
    ) -> Result<package::ChangeSetItemSummaryConnection, gql::ChangeSetError> {
        let id = parse_numeric_id(id.as_str(), "change set")?;
        let limit = page_size(first)?;
        let search = normalize_filter(search);
        let binding = digest(&json!(["items", id, search, problems_only]));
        let after_id = decode_cursor(after.clone(), &binding)?
            .map(|v| {
                v.as_i64()
                    .ok_or_else(|| conflict("BAD_CURSOR", "invalid item cursor"))
            })
            .transpose()?
            .unwrap_or(0);
        let predicate = "change_set_id=$1 AND ($2::text IS NULL OR item_key ILIKE '%'||$2||'%') AND (NOT $3 OR validation_error IS NOT NULL)";
        let total = count(
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe::<&str>(&format!(
                "SELECT COUNT(*) FROM change_set_items WHERE {predicate}"
            )))
            .bind(id)
            .bind(&search)
            .bind(problems_only)
            .fetch_one(&self.pool)
            .await
            .map_err(query_error)?,
        )?;
        // Field counts are computed in SQL; no snapshot is sent in a summary connection.
        let rows=sqlx::query(sqlx::AssertSqlSafe::<&str>(&format!("SELECT id,item_key,action,validation_error,(SELECT COUNT(*) FROM jsonb_object_keys(COALESCE(before_snapshot,'{{}}'::jsonb)||COALESCE(after_snapshot,'{{}}'::jsonb)) k WHERE before_snapshot->k IS DISTINCT FROM after_snapshot->k) AS changed_fields FROM change_set_items WHERE {predicate} AND id>$4 ORDER BY id LIMIT $5"))).bind(id).bind(search).bind(problems_only).bind(after_id).bind(limit+1).fetch_all(&self.pool).await.map_err(query_error)?;
        let next = rows.len() > limit as usize;
        let mut edges = Vec::new();
        for r in rows.into_iter().take(limit as usize) {
            let n: i64 = r.get("id");
            let key: String = r.get("item_key");
            let error: Option<String> = r.get("validation_error");
            edges.push(package::ChangeSetItemSummaryEdge {
                cursor: cursor(&binding, json!(n)),
                node: package::ChangeSetItemSummary {
                    id: ID(n.to_string()),
                    item_key: key.clone(),
                    action: parse_action(&r.get::<String, _>("action"))?,
                    model_label: Some(key.clone()),
                    changed_field_count: count(r.get("changed_fields"))?,
                    validation_state: if error.is_some() {
                        package::ValidationState::Invalid
                    } else {
                        package::ValidationState::Valid
                    },
                    issues: error
                        .into_iter()
                        .map(|m| issue(Some(key.clone()), m))
                        .collect(),
                },
            });
        }
        let cursors = edges.iter().map(|e| e.cursor.clone()).collect::<Vec<_>>();
        Ok(package::ChangeSetItemSummaryConnection {
            edges,
            page_info: page_info(&cursors, next, after.is_some()),
            total_count: total,
        })
    }
    async fn get_item(
        &self,
        id: ID,
        item_id: ID,
    ) -> Result<Option<package::ChangeSetItemDetail>, gql::ChangeSetError> {
        let id = parse_numeric_id(id.as_str(), "change set")?;
        let item_id = parse_numeric_id(item_id.as_str(), "item")?;
        let row=sqlx::query_as::<_,ChangeSetItemRow>(sqlx::AssertSqlSafe::<&str>(&format!("SELECT {CHANGE_SET_ITEM_COLUMNS} FROM change_set_items WHERE change_set_id=$1 AND id=$2"))).bind(id).bind(item_id).fetch_optional(&self.pool).await.map_err(query_error)?;
        let Some(row) = row else { return Ok(None) };
        let cs = sqlx::query_as::<_, (String, String)>(
            "SELECT kind,scope_id FROM change_sets WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(query_error)?;
        let current=match cs.0.as_str(){"provider_price"=>{let mut tx=self.pool.begin().await.map_err(storage_error)?;let rows=conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(&mut tx,parse_numeric_id(&cs.1,"channel")?).await.map_err(storage_error)?;let v=rows.iter().find(|p|p.model_id==row.item_key).map(base_price_snapshot);tx.commit().await.map_err(storage_error)?;v},"retail_price"=>sqlx::query_scalar::<_,SqlJson<Value>>("SELECT i.price FROM price_book_items i JOIN price_book_versions v ON v.id=i.price_book_version_id WHERE v.price_book_id=$1 AND v.status='published' AND i.public_model_id=$2 ORDER BY v.version DESC LIMIT 1").bind(parse_numeric_id(&cs.1,"price book")?).bind(parse_numeric_id(&row.item_key,"model")?).fetch_optional(&self.pool).await.map_err(query_error)?.map(|v|v.0),"model_mapping"=>mapping_current(&self.pool,&row).await?,_=>None};
        let matches = current == row.before_snapshot.as_ref().map(|v| v.0.clone());
        let issues = row
            .validation_error
            .clone()
            .into_iter()
            .map(|m| issue(Some(row.item_key.clone()), m))
            .collect();
        Ok(Some(package::ChangeSetItemDetail {
            item: row.into_graphql()?,
            current_snapshot: current.map(async_graphql::Json),
            current_matches_before: matches,
            issues,
        }))
    }
    async fn list_events(
        &self,
        id: ID,
        after: Option<CursorScalar>,
        first: i32,
    ) -> Result<package::ChangeSetEventConnection, gql::ChangeSetError> {
        let id = parse_numeric_id(id.as_str(), "change set")?;
        let limit = page_size(first)?;
        let binding = format!("events:{id}");
        let after_id = decode_cursor(after.clone(), &binding)?
            .map(|v| {
                v.as_i64()
                    .ok_or_else(|| conflict("BAD_CURSOR", "invalid event cursor"))
            })
            .transpose()?
            .unwrap_or(0);
        let total = count(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM change_set_events WHERE change_set_id=$1",
            )
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(query_error)?,
        )?;
        let rows=sqlx::query_as::<_,ChangeSetEventRow>("SELECT id,change_set_id,event_type,actor_type,actor_id,detail,created_at FROM change_set_events WHERE change_set_id=$1 AND id>$2 ORDER BY id LIMIT $3").bind(id).bind(after_id).bind(limit+1).fetch_all(&self.pool).await.map_err(query_error)?;
        let next = rows.len() > limit as usize;
        let edges = rows
            .into_iter()
            .take(limit as usize)
            .map(|r| package::ChangeSetEventEdge {
                cursor: cursor(&binding, json!(r.id)),
                node: gql::ChangeSetEvent {
                    id: ID(r.id.to_string()),
                    event_type: r.event_type,
                    actor_type: r.actor_type,
                    actor_id: r.actor_id.map(|n| ID(n.to_string())),
                    detail: async_graphql::Json(r.detail.0),
                    created_at: TimeScalar(r.created_at),
                },
            })
            .collect::<Vec<_>>();
        let cursors = edges.iter().map(|e| e.cursor.clone()).collect::<Vec<_>>();
        Ok(package::ChangeSetEventConnection {
            edges,
            page_info: page_info(&cursors, next, after.is_some()),
            total_count: total,
        })
    }
    async fn find_by_operation(
        &self,
        viewer: package::PackageViewer,
        op: ID,
        kind: gql::ChangeSetKind,
        scope_type: String,
        scope_id: ID,
    ) -> Result<Option<package::ChangeSetDetail>, gql::ChangeSetError> {
        let id=sqlx::query_scalar::<_,i64>("SELECT id FROM change_sets WHERE operation_id=$1 AND kind=$2 AND scope_type=$3 AND scope_id=$4").bind(operation(&op)?).bind(kind.as_str()).bind(scope_type).bind(parse_numeric_id(scope_id.as_str(),"scope")?.to_string()).fetch_optional(&self.pool).await.map_err(query_error)?;
        match id {
            Some(n) => detail(&self.pool, viewer, n).await,
            None => Ok(None),
        }
    }
    async fn find_provider_operation(
        &self,
        viewer: package::PackageViewer,
        op: ID,
    ) -> Result<Option<package::CreateChangeSetResult>, gql::ChangeSetError> {
        let row=sqlx::query_scalar::<_,SqlJson<Value>>("SELECT stage_results->'providerPriceEdit' FROM channel_operation_receipts WHERE operation_id=$1 AND operation_type='PROVIDER_PRICE_EDIT'").bind(operation(&op)?).fetch_optional(&self.pool).await.map_err(storage_error)?;
        let Some(r) = row else { return Ok(None) };
        let id = match (r.0["outcome"].as_str(), r.0.get("changeSetID")) {
            (Some("CREATED"), Some(value)) => Some(value.as_i64().ok_or_else(|| {
                gql::ChangeSetError::Storage("invalid provider price receipt package ID".into())
            })?),
            (Some("NO_CHANGE"), Some(Value::Null)) => None,
            _ => {
                return Err(gql::ChangeSetError::Storage(
                    "invalid provider price receipt".into(),
                ));
            }
        };
        let d = match id {
            Some(n) => Some(committed_provider_detail(&self.pool, viewer, n).await?),
            None => None,
        };
        Ok(Some(package::CreateChangeSetResult {
            operation_id: op,
            outcome: if id.is_some() {
                package::CreateChangeSetOutcome::Created
            } else {
                package::CreateChangeSetOutcome::NoChange
            },
            change_set_detail: d,
        }))
    }
    async fn provider_edit_base(
        &self,
        id: ID,
    ) -> Result<package::ProviderPriceEditBase, gql::ChangeSetError> {
        provider_base(&self.pool, parse_numeric_id(id.as_str(), "channel")?).await
    }
    async fn retail_edit_base(
        &self,
        id: ID,
    ) -> Result<package::RetailPriceEditBase, gql::ChangeSetError> {
        retail_base(&self.pool, parse_numeric_id(id.as_str(), "price book")?).await
    }

    async fn create_provider(
        &self,
        actor: i64,
        channel: ID,
        op: ID,
        expected: String,
        input: Vec<conduit_admin_graphql::model_ext::SaveChannelModelPriceInput>,
        correction: Option<package::ChangeSetCorrectionInput>,
    ) -> Result<package::CreateChangeSetResult, gql::ChangeSetError> {
        let channel = parse_numeric_id(channel.as_str(), "channel")?;
        let op_id = operation(&op)?;
        let prepared = prepare_provider(input)?;
        let origin = if correction.is_some() {
            "CORRECTION"
        } else {
            "MANUAL_EDIT"
        };
        let fp = digest(
            &json!({"channel":channel,"actor":actor,"origin":origin,"expectedBase":expected,"prices":prepared.iter().map(|p|json!([p.key,p.currency,p.after])).collect::<Vec<_>>(),"correction":correction.as_ref().map(|c|json!([c.source_change_set_id.as_str(),c.expected_source_package_revision,c.excluded_item_keys,c.reason]))}),
        );
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        lock_operation(&mut tx, op_id).await?;
        if let Some(r)=sqlx::query_as::<_,(String,String,i64)>("SELECT request_fingerprint,operation_type,channel_id FROM channel_operation_receipts WHERE operation_id=$1").bind(op_id).fetch_optional(&mut *tx).await.map_err(storage_error)?{
            if r!=(fp.clone(),"PROVIDER_PRICE_EDIT".into(),channel){return Err(conflict("OPERATION_CONFLICT","operationID already belongs to another intent"));}
            tx.commit().await.map_err(storage_error)?;return package::ChangePackageServices::find_provider_operation(self,package::PackageViewer{actor_user_id:actor,can_write:true},op).await?.ok_or_else(||conflict("OPERATION_CONFLICT","operation receipt is missing"));
        }
        lock_scope(&mut tx, "provider_price", "channel", &channel.to_string()).await?;
        if let Some(c) = &correction {
            let source_id = parse_numeric_id(c.source_change_set_id.as_str(), "source package")?;
            lock_change_set(&mut tx, source_id).await?;
        }
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(storage_error)?;
        let base = provider_base_in_tx(&mut tx, channel).await?;
        if base.revision != expected {
            return Err(conflict(
                "SOURCE_CHANGED",
                "formal price list or accounting settings changed; reload edit base",
            ));
        }
        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(channel)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| conflict("SOURCE_CHANGED", "channel not found"))?;
        let correction_source = if let Some(c) = &correction {
            let source_id = parse_numeric_id(c.source_change_set_id.as_str(), "source package")?;
            let source = lock_change_set(&mut tx, source_id).await?;
            expect_revision(&mut tx, source_id, &c.expected_source_package_revision).await?;
            if source.kind != "provider_price"
                || source.scope_id != channel.to_string()
                || !matches!(source.status.as_str(), "invalid" | "superseded")
                || c.reason.trim().is_empty()
            {
                return Err(conflict(
                    "STATE_CHANGED",
                    "correction requires an invalid or superseded package in this channel and a reason",
                ));
            }
            for key in &c.excluded_item_keys {
                if !sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM change_set_items WHERE change_set_id=$1 AND item_key=$2)").bind(source_id).bind(key).fetch_one(&mut *tx).await.map_err(storage_error)?{return Err(conflict("VALIDATION_FAILED","excluded item does not belong to source package"));}
            }
            Some(
                json!({"derivedFromSourceIDs":[source_id],"excludedItemKeys":c.excluded_item_keys,"reason":c.reason}),
            )
        } else {
            None
        };
        let existing =
            conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(&mut tx, channel)
                .await
                .map_err(storage_error)?;
        let proposed = provider_changes(
            &existing,
            &prepared,
            &base.accounting_currency,
            &base.accounting_settings_version,
            correction_source.clone(),
        )?;
        let now = Utc::now();
        let change_set_id = if proposed.is_empty() {
            None
        } else {
            let id=sqlx::query_scalar::<_,i64>("INSERT INTO change_sets(operation_id,kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,created_at,updated_at) VALUES($1,'provider_price','channel',$2,$3,'draft',$4,$5,$6,$7,$7) RETURNING id").bind(op_id).bind(channel.to_string()).bind(format!("Procurement price edit: {name}")).bind(&expected).bind(&base.accounting_settings_version).bind(actor).bind(now).fetch_one(&mut *tx).await.map_err(storage_error)?;
            write_proposals(&mut tx, id, &proposed, now).await?;
            insert_event(&mut tx,id,"created","user",Some(actor),json!({"origin":origin,"operationID":op_id,"requestFingerprint":fp,"itemCount":proposed.len(),"correction":correction_source}),now).await?;
            Some(id)
        };
        sqlx::query("INSERT INTO channel_operation_receipts(operation_id,channel_id,operation_type,origin,created_by,request_fingerprint,state,stage_results,created_at,updated_at) VALUES($1,$2,'PROVIDER_PRICE_EDIT',$3,$4,$5,'SUCCEEDED',$6,$7,$7)").bind(op_id).bind(channel).bind(origin).bind(actor).bind(fp).bind(SqlJson(json!({"providerPriceEdit":{"outcome":if change_set_id.is_some(){"CREATED"}else{"NO_CHANGE"},"changeSetID":change_set_id}}))).bind(now).execute(&mut *tx).await.map_err(storage_error)?;
        tx.commit().await.map_err(storage_error)?;
        Ok(package::CreateChangeSetResult {
            operation_id: op,
            outcome: if change_set_id.is_some() {
                package::CreateChangeSetOutcome::Created
            } else {
                package::CreateChangeSetOutcome::NoChange
            },
            change_set_detail: match change_set_id {
                Some(id) => Some(
                    committed_provider_detail(
                        &self.pool,
                        package::PackageViewer {
                            actor_user_id: actor,
                            can_write: true,
                        },
                        id,
                    )
                    .await?,
                ),
                None => None,
            },
        })
    }
    async fn save_provider(
        &self,
        actor: i64,
        id: ID,
        expected: String,
        input: Vec<conduit_admin_graphql::model_ext::SaveChannelModelPriceInput>,
    ) -> Result<package::ChangeSetDetail, gql::ChangeSetError> {
        let id = parse_numeric_id(id.as_str(), "package")?;
        let prepared = prepare_provider(input)?;
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        let _policy = crate::wiring_governance::lock_policy(&mut tx)
            .await
            .map_err(gql::ChangeSetError::Invalid)?;
        let cs = lock_package(&mut tx, id).await?;
        expect_revision(&mut tx, id, &expected).await?;
        if cs.kind != "provider_price" || cs.status != "draft" {
            return Err(conflict(
                "STATE_CHANGED",
                "only the specified provider-price draft is editable",
            ));
        }
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(storage_error)?;
        let channel = parse_numeric_id(&cs.scope_id, "channel")?;
        let base = provider_base_in_tx(&mut tx, channel).await?;
        if base.revision != cs.base_revision {
            return Err(conflict(
                "SOURCE_CHANGED",
                "formal prices changed since this draft was created",
            ));
        }
        let existing =
            conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(&mut tx, channel)
                .await
                .map_err(storage_error)?;
        let proposals = provider_changes(
            &existing,
            &prepared,
            &base.accounting_currency,
            &base.accounting_settings_version,
            None,
        )?;
        sqlx::query("DELETE FROM change_set_items WHERE change_set_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
        write_proposals(&mut tx, id, &proposals, Utc::now()).await?;
        sqlx::query("UPDATE change_sets SET updated_at=now() WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
        insert_event(
            &mut tx,
            id,
            "items_replaced",
            "user",
            Some(actor),
            json!({"itemCount":proposals.len()}),
            Utc::now(),
        )
        .await?;
        tx.commit().await.map_err(storage_error)?;
        required_detail(&self.pool, actor, id).await
    }
    async fn create_retail(
        &self,
        actor: i64,
        book: ID,
        op: ID,
        expected: String,
    ) -> Result<package::ChangeSetDetail, gql::ChangeSetError> {
        let book = parse_numeric_id(book.as_str(), "price book")?;
        let op = operation(&op)?;
        let fp = digest(&json!([actor, book, expected, "MANUAL_EDIT"]));
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        lock_operation(&mut tx, op).await?;
        lock_scope(&mut tx, "retail_price", "price_book", &book.to_string()).await?;
        if let Some((id,stored))=sqlx::query_as::<_,(i64,Option<String>)>("SELECT cs.id,(SELECT detail->>'requestFingerprint' FROM change_set_events WHERE change_set_id=cs.id AND event_type='created' ORDER BY id LIMIT 1) FROM change_sets cs WHERE operation_id=$1 AND kind='retail_price' AND scope_type='price_book' AND scope_id=$2").bind(op).bind(book.to_string()).fetch_optional(&mut *tx).await.map_err(storage_error)?{if stored.as_deref()!=Some(fp.as_str()){return Err(conflict("OPERATION_CONFLICT","operationID belongs to another retail intent"));}tx.commit().await.map_err(storage_error)?;return required_detail(&self.pool,actor,id).await;}
        if sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM change_sets WHERE operation_id=$1)",
        )
        .bind(op)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?
        {
            return Err(conflict(
                "OPERATION_CONFLICT",
                "operationID belongs to another scope",
            ));
        }
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(storage_error)?;
        let base = retail_base_in_tx(&mut tx, book).await?;
        if expected != base.revision {
            return Err(conflict("SOURCE_CHANGED", "published price base changed"));
        }
        let (name, currency) = sqlx::query_as::<_, (String, String)>(
            "SELECT name,currency FROM price_books WHERE id=$1 FOR UPDATE",
        )
        .bind(book)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?;
        if !currency.eq_ignore_ascii_case(&base.accounting_currency) {
            return Err(conflict(
                "VALIDATION_FAILED",
                "price book must use the accounting currency",
            ));
        }
        let published=sqlx::query_scalar::<_,String>("SELECT reference_id FROM price_book_versions WHERE price_book_id=$1 AND status='published' ORDER BY version DESC LIMIT 1").bind(book).fetch_optional(&mut *tx).await.map_err(storage_error)?.unwrap_or_default();
        let id=sqlx::query_scalar::<_,i64>("INSERT INTO change_sets(operation_id,kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,created_at,updated_at) VALUES($1,'retail_price','price_book',$2,$3,'draft',$4,$5,$6,now(),now()) RETURNING id").bind(op).bind(book.to_string()).bind(format!("Retail price edit: {name}")).bind(published).bind(base.accounting_settings_version).bind(actor).fetch_one(&mut *tx).await.map_err(storage_error)?;
        insert_event(&mut tx,id,"created","user",Some(actor),json!({"origin":"MANUAL_EDIT","operationID":op,"requestFingerprint":fp,"editBaseRevision":expected}),Utc::now()).await?;
        tx.commit().await.map_err(storage_error)?;
        required_detail(&self.pool, actor, id).await
    }
    async fn save_retail_item(
        &self,
        actor: i64,
        input: gql::SaveRetailPriceChangeSetItemInput,
    ) -> Result<package::SaveRetailPriceChangeSetItemResult, gql::ChangeSetError> {
        let id = parse_numeric_id(input.change_set_id.as_str(), "package")?;
        let model = parse_numeric_id(input.public_model_id.as_str(), "public model")?;
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        let cs = lock_package(&mut tx, id).await?;
        expect_revision(&mut tx, id, &input.expected_revision).await?;
        if cs.kind != "retail_price" || cs.status != "draft" {
            return Err(conflict("STATE_CHANGED", "only a retail draft is editable"));
        }
        // A human override no longer belongs to automatic formula refresh.
        sqlx::query("UPDATE change_sets SET formula_id=NULL WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
        let current_reference=sqlx::query_scalar::<_,String>("SELECT reference_id FROM price_book_versions WHERE price_book_id=$1 AND status='published' ORDER BY version DESC LIMIT 1").bind(parse_numeric_id(&cs.scope_id,"price book")?).fetch_optional(&mut *tx).await.map_err(storage_error)?.unwrap_or_default();
        if current_reference != cs.base_revision {
            return Err(conflict("SOURCE_CHANGED", "published retail base changed"));
        }
        let (key, model_type) = sqlx::query_as::<_, (String, String)>(
            "SELECT model_id,\"type\" FROM models WHERE id=$1 AND deleted_at=0",
        )
        .bind(model)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?;
        let before=sqlx::query_scalar::<_,SqlJson<Value>>("SELECT i.price FROM price_book_items i JOIN price_book_versions v ON v.id=i.price_book_version_id WHERE v.reference_id=$1 AND v.price_book_id=$2 AND i.public_model_id=$3").bind(&cs.base_revision).bind(parse_numeric_id(&cs.scope_id,"price book")?).bind(model).fetch_optional(&mut *tx).await.map_err(storage_error)?;
        let after = input.price.0;
        let mut error = validate_price_for_type(&after, price_request_type(&model_type)).err();
        let mut source = json!({"publicModelID":model,"publicModelKey":key});
        if error.is_none() && contains_expression_price(&after) {
            match crate::wiring_postgres_pricing_admission::expression_retail_route_warnings(
                &mut tx, model,
            )
            .await
            {
                Ok(excluded) if !excluded.is_empty() => {
                    source["warnings"] = json!([{
                        "code":"EXPR_REQUIRES_UNPROVEN_USAGE_FACTS",
                        "message":"These channels will be excluded at expression-pricing admission.",
                        "excludedChannelIDs":excluded,
                    }]);
                }
                Ok(_) => {}
                Err(message) => error = Some(message),
            }
        }
        let item_id=sqlx::query_scalar::<_,i64>("INSERT INTO change_set_items(change_set_id,item_key,action,before_snapshot,after_snapshot,source_snapshot,validation_error,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,now(),now()) ON CONFLICT(change_set_id,item_key) DO UPDATE SET action=EXCLUDED.action,before_snapshot=EXCLUDED.before_snapshot,after_snapshot=EXCLUDED.after_snapshot,source_snapshot=EXCLUDED.source_snapshot,validation_error=EXCLUDED.validation_error,updated_at=now() RETURNING id").bind(id).bind(model.to_string()).bind(if before.is_some(){"update"}else{"create"}).bind(before.clone()).bind(SqlJson(after.clone())).bind(SqlJson(source)).bind(error).fetch_one(&mut *tx).await.map_err(storage_error)?;
        let item = load_item_in_tx(&mut tx, item_id).await?.into_graphql()?;
        let unchanged = before.as_ref().is_some_and(|v| v.0 == after);
        if unchanged {
            sqlx::query("DELETE FROM change_set_items WHERE id=$1")
                .bind(item_id)
                .execute(&mut *tx)
                .await
                .map_err(storage_error)?;
        }
        insert_event(
            &mut tx,
            id,
            if unchanged {
                "item_removed"
            } else {
                "item_saved"
            },
            "user",
            Some(actor),
            json!({"itemKey":model.to_string(),"itemID":item_id,"unchanged":unchanged}),
            Utc::now(),
        )
        .await?;
        sqlx::query("UPDATE change_sets SET updated_at=now() WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
        let change_set_revision = revision(&mut tx, id).await?;
        tx.commit().await.map_err(storage_error)?;
        Ok(package::SaveRetailPriceChangeSetItemResult {
            item,
            change_set_revision,
        })
    }
    async fn transition(
        &self,
        actor: i64,
        id: ID,
        expected: String,
        action: package::ChangeSetAvailableAction,
        note: Option<String>,
    ) -> Result<package::ChangeSetDetail, gql::ChangeSetError> {
        let id = parse_numeric_id(id.as_str(), "package")?;
        let note = normalize_note(note);
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        let policy = crate::wiring_governance::lock_policy(&mut tx)
            .await
            .map_err(gql::ChangeSetError::Invalid)?;
        let mut cs = lock_package(&mut tx, id).await?;
        let mut policy = policy;
        let requires_review = if cs.scope_type == "governed" {
            crate::wiring_governance_commands::requires_review(&mut tx, cs.id, &policy)
                .await
                .map_err(gql::ChangeSetError::Invalid)?
        } else {
            policy.requires(&cs.kind)
                || (cs.kind == "model_mapping" && policy.requires("model_supply"))
        };
        policy.enabled = requires_review;
        policy.review_kinds = if requires_review {
            vec![cs.kind.clone()]
        } else {
            vec![]
        };
        if cs.scope_type == "governed" {
            policy.legacy_review_scopes = false;
        }
        expect_revision(&mut tx, id, &expected).await?;
        let now = Utc::now();
        // The convenience action performs review and publication in this same
        // transaction. It always requires both explicit duties, including for
        // historical packages with legacy review privileges.
        let action = if action == package::ChangeSetAvailableAction::ApproveApply {
            crate::wiring_governance::authorize(&mut tx, actor, "review_changes")
                .await
                .map_err(gql::ChangeSetError::Invalid)?;
            crate::wiring_governance::authorize(&mut tx, actor, "publish_changes")
                .await
                .map_err(gql::ChangeSetError::Invalid)?;
            if cs.status != "pending_review" {
                return Err(conflict(
                    "STATE_CHANGED",
                    "only a pending package can be approved and published",
                ));
            }
            if cs.scope_type == "governed" {
                crate::wiring_governance_commands::authorize_package(&mut tx, cs.id, actor)
                    .await
                    .map_err(gql::ChangeSetError::Invalid)?;
            }
            crate::wiring_governance::review_permission(
                &mut tx,
                &policy,
                actor,
                cs.created_by,
                &cs.kind,
                false,
            )
            .await
            .map_err(gql::ChangeSetError::Invalid)?;
            let issues = validate_items(&mut tx, &cs).await?;
            if !issues.is_empty() {
                let stale = issues.iter().any(|v| v["code"] == "SOURCE_CHANGED");
                if stale {
                    mark_reviewed(&mut tx, id, "superseded", actor, note.as_deref(), None, now)
                        .await?;
                } else {
                    mark_invalid(
                        &mut tx,
                        id,
                        actor,
                        note.as_deref(),
                        "one or more package items are invalid",
                        now,
                    )
                    .await?;
                }
                insert_event(
                    &mut tx,
                    id,
                    "validation_failed",
                    "user",
                    Some(actor),
                    json!({"issues":issues}),
                    now,
                )
                .await?;
                tx.commit().await.map_err(storage_error)?;
                return Err(conflict(
                    if stale {
                        "SOURCE_CHANGED"
                    } else {
                        "VALIDATION_FAILED"
                    },
                    "the package was not approved or published",
                ));
            }
            mark_reviewed(&mut tx, id, "approved", actor, note.as_deref(), None, now).await?;
            insert_event(
                &mut tx,
                id,
                "approved",
                "user",
                Some(actor),
                json!({"convenience":true}),
                now,
            )
            .await?;
            cs.status = "approved".into();
            cs.reviewed_by = Some(actor);
            cs.reviewed_at = Some(now);
            package::ChangeSetAvailableAction::Publish
        } else {
            action
        };
        match action {
            package::ChangeSetAvailableAction::Submit => {
                if cs.scope_type == "governed" {
                    crate::wiring_governance_commands::authorize_package(&mut tx, cs.id, actor)
                        .await
                        .map_err(gql::ChangeSetError::Invalid)?;
                } else {
                    crate::wiring_governance::authorize(
                        &mut tx,
                        actor,
                        crate::wiring_governance::domain(&cs.kind),
                    )
                    .await
                    .map_err(gql::ChangeSetError::Invalid)?;
                }
                if cs.created_by.is_some_and(|maker| maker != actor) {
                    return Err(conflict(
                        "FORBIDDEN",
                        "only the maker may submit this package",
                    ));
                }
                if cs.status != "draft" {
                    return Err(conflict("STATE_CHANGED", "only a draft can be submitted"));
                }
                let invalid = validate_items(&mut tx, &cs).await?;
                if !invalid.is_empty() {
                    insert_event(
                        &mut tx,
                        id,
                        "validation_failed",
                        "user",
                        Some(actor),
                        json!({"issues":invalid}),
                        now,
                    )
                    .await?;
                    tx.commit().await.map_err(storage_error)?;
                    return Err(conflict(
                        "VALIDATION_FAILED",
                        "all package items must be valid and the package nonempty",
                    ));
                }
                let actors: Vec<i64> = sqlx::query_scalar(
                    "SELECT id FROM users WHERE deleted_at=0 AND status='activated'",
                )
                .fetch_all(&mut *tx)
                .await
                .map_err(storage_error)?;
                let mut eligible = false;
                for reviewer in actors {
                    let domain = if cs.scope_type == "governed" {
                        crate::wiring_governance_commands::authorize_package(
                            &mut tx, cs.id, reviewer,
                        )
                        .await
                        .is_ok()
                    } else {
                        crate::wiring_governance::authorize(
                            &mut tx,
                            reviewer,
                            crate::wiring_governance::domain(&cs.kind),
                        )
                        .await
                        .is_ok()
                    };
                    if domain
                        && crate::wiring_governance::review_permission(
                            &mut tx,
                            &policy,
                            reviewer,
                            cs.created_by,
                            &cs.kind,
                            false,
                        )
                        .await
                        .is_ok()
                    {
                        eligible = true;
                        break;
                    }
                }
                if !eligible {
                    return Err(conflict(
                        "NO_ELIGIBLE_REVIEWER",
                        "no qualified reviewer is available for this package",
                    ));
                }
                sqlx::query("UPDATE change_sets SET status='pending_review',submitted_by=$2,submitted_at=$3,updated_at=$3 WHERE id=$1").bind(id).bind(actor).bind(now).execute(&mut *tx).await.map_err(storage_error)?;
                insert_event(
                    &mut tx,
                    id,
                    "submitted",
                    "user",
                    Some(actor),
                    json!({}),
                    now,
                )
                .await?;
            }
            package::ChangeSetAvailableAction::Reject => {
                if cs.scope_type == "governed" {
                    crate::wiring_governance_commands::authorize_package(&mut tx, cs.id, actor)
                        .await
                        .map_err(gql::ChangeSetError::Invalid)?;
                }
                crate::wiring_governance::review_permission(
                    &mut tx,
                    &policy,
                    actor,
                    cs.created_by,
                    &cs.kind,
                    false,
                )
                .await
                .map_err(gql::ChangeSetError::Invalid)?;
                if !matches!(cs.status.as_str(), "pending_review" | "invalid") {
                    return Err(conflict(
                        "STATE_CHANGED",
                        "only pending or invalid packages can be rejected",
                    ));
                }
                mark_reviewed(&mut tx, id, "rejected", actor, note.as_deref(), None, now).await?;
                insert_event(&mut tx, id, "rejected", "user", Some(actor), json!({}), now).await?;
            }
            package::ChangeSetAvailableAction::Approve => {
                if cs.status != "pending_review" {
                    return Err(conflict(
                        "STATE_CHANGED",
                        "only a pending package can be reviewed",
                    ));
                }
                if cs.scope_type == "governed" {
                    crate::wiring_governance_commands::authorize_package(&mut tx, cs.id, actor)
                        .await
                        .map_err(gql::ChangeSetError::Invalid)?;
                }
                crate::wiring_governance::review_permission(
                    &mut tx,
                    &policy,
                    actor,
                    cs.created_by,
                    &cs.kind,
                    false,
                )
                .await
                .map_err(gql::ChangeSetError::Invalid)?;
                let issues = validate_items(&mut tx, &cs).await?;
                if !issues.is_empty() {
                    let stale = issues.iter().any(|v| v["code"] == "SOURCE_CHANGED");
                    if stale {
                        mark_reviewed(&mut tx, id, "superseded", actor, note.as_deref(), None, now)
                            .await?;
                    } else {
                        mark_invalid(
                            &mut tx,
                            id,
                            actor,
                            note.as_deref(),
                            "one or more package items are invalid",
                            now,
                        )
                        .await?;
                    }
                    insert_event(
                        &mut tx,
                        id,
                        "validation_failed",
                        "user",
                        Some(actor),
                        json!({"issues":issues}),
                        now,
                    )
                    .await?;
                    tx.commit().await.map_err(storage_error)?;
                    return Err(conflict(
                        if stale {
                            "SOURCE_CHANGED"
                        } else {
                            "VALIDATION_FAILED"
                        },
                        "the package was not approved",
                    ));
                }
                mark_reviewed(&mut tx, id, "approved", actor, note.as_deref(), None, now).await?;
                insert_event(&mut tx, id, "approved", "user", Some(actor), json!({}), now).await?;
            }
            package::ChangeSetAvailableAction::Publish => {
                let publishing = cs.status == "approved";
                let direct_publish = cs.status == "draft" && !requires_review;
                if !publishing && !direct_publish {
                    return Err(conflict(
                        "STATE_CHANGED",
                        "package is no longer pending review",
                    ));
                }
                if cs.scope_type == "governed" {
                    crate::wiring_governance_commands::authorize_package(&mut tx, cs.id, actor)
                        .await
                        .map_err(gql::ChangeSetError::Invalid)?;
                }
                crate::wiring_governance::review_permission(
                    &mut tx,
                    &policy,
                    actor,
                    cs.created_by,
                    &cs.kind,
                    true,
                )
                .await
                .map_err(gql::ChangeSetError::Invalid)?;
                if let Some(maker) = cs.created_by {
                    if cs.scope_type == "governed" {
                        crate::wiring_governance_commands::authorize_package(&mut tx, cs.id, maker)
                            .await
                            .map_err(gql::ChangeSetError::Invalid)?;
                    } else {
                        crate::wiring_governance::authorize(
                            &mut tx,
                            maker,
                            crate::wiring_governance::domain(&cs.kind),
                        )
                        .await
                        .map_err(gql::ChangeSetError::Invalid)?;
                    }
                }
                if publishing {
                    let reviewer = cs.reviewed_by.ok_or_else(|| {
                        conflict("STATE_CHANGED", "approved package has no reviewer")
                    })?;
                    if cs.scope_type == "governed" {
                        crate::wiring_governance_commands::authorize_package(
                            &mut tx, cs.id, reviewer,
                        )
                        .await
                        .map_err(gql::ChangeSetError::Invalid)?;
                    }
                    crate::wiring_governance::review_permission(
                        &mut tx,
                        &policy,
                        reviewer,
                        cs.created_by,
                        &cs.kind,
                        false,
                    )
                    .await
                    .map_err(gql::ChangeSetError::Invalid)?;
                }
                let invalid = validate_items(&mut tx, &cs).await?;
                if !invalid.is_empty() {
                    let stale = invalid.iter().any(|v| v["code"] == "SOURCE_CHANGED");
                    if stale {
                        mark_reviewed(&mut tx, id, "superseded", actor, note.as_deref(), None, now)
                            .await?;
                    } else {
                        mark_invalid(
                            &mut tx,
                            id,
                            actor,
                            note.as_deref(),
                            "one or more package items are invalid",
                            now,
                        )
                        .await?;
                    }
                    insert_event(
                        &mut tx,
                        id,
                        "validation_failed",
                        "user",
                        Some(actor),
                        json!({"issues":invalid}),
                        now,
                    )
                    .await?;
                    tx.commit().await.map_err(storage_error)?;
                    return Err(conflict(
                        if stale {
                            "SOURCE_CHANGED"
                        } else {
                            "VALIDATION_FAILED"
                        },
                        "the package was not applied; reload its persisted state",
                    ));
                }
                if direct_publish {
                    sqlx::query("UPDATE change_sets SET status='pending_review' WHERE id=$1")
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(storage_error)?;
                }
                let applied = {
                    let mut savepoint = tx.begin().await.map_err(storage_error)?;
                    sqlx::query("SELECT set_config('conduit.governance_publish',$1,true)")
                        .bind(id.to_string())
                        .execute(&mut *savepoint)
                        .await
                        .map_err(storage_error)?;
                    let mut governed_targets = Vec::new();
                    let result = if cs.scope_type == "governed" {
                        crate::wiring_governance_commands::apply(
                            &mut savepoint,
                            cs.id,
                            actor,
                            &policy,
                        )
                        .await
                        .map(|targets| {
                            governed_targets = targets;
                            ("governed".to_string(), cs.id.to_string())
                        })
                        .map_err(ApplyFailure::Invalid)
                    } else {
                        match cs.kind.as_str() {
                            "provider_price" => {
                                apply_provider_price(&mut savepoint, &cs, actor).await
                            }
                            "model_mapping" => apply_model_mapping(&mut savepoint, &cs).await,
                            "retail_price" => apply_retail_price(&mut savepoint, &cs, actor).await,
                            _ => Err(ApplyFailure::Invalid("unsupported package kind".into())),
                        }
                    };
                    match result {
                        Ok(v) => {
                            savepoint.commit().await.map_err(storage_error)?;
                            Ok((v, governed_targets))
                        }
                        Err(e) => {
                            savepoint.rollback().await.map_err(storage_error)?;
                            Err(e)
                        }
                    }
                };
                let ((target_type, target_id), governed_targets) = match applied {
                    Ok(v) => v,
                    Err(ApplyFailure::Storage(m)) => return Err(gql::ChangeSetError::Storage(m)),
                    Err(error) => {
                        let (status, message) = match error {
                            ApplyFailure::Superseded(m) => ("superseded", m),
                            ApplyFailure::Invalid(m) => ("invalid", m),
                            ApplyFailure::Storage(_) => unreachable!(),
                        };
                        if status == "invalid" {
                            mark_invalid(&mut tx, id, actor, note.as_deref(), &message, now)
                                .await?;
                        } else {
                            mark_reviewed(&mut tx, id, status, actor, note.as_deref(), None, now)
                                .await?;
                        }
                        // The savepoint has rolled back all formal writes. Persist the actual failing item diagnosis.
                        persist_failure_issues(&mut tx, &cs, &message).await?;
                        insert_event(
                            &mut tx,
                            id,
                            if status == "invalid" {
                                "validation_failed"
                            } else {
                                "superseded"
                            },
                            "user",
                            Some(actor),
                            json!({"reason":message}),
                            now,
                        )
                        .await?;
                        tx.commit().await.map_err(storage_error)?;
                        return Err(conflict(
                            if status == "invalid" {
                                "VALIDATION_FAILED"
                            } else {
                                "SOURCE_CHANGED"
                            },
                            message,
                        ));
                    }
                };
                let targets = if cs.scope_type == "governed" {
                    governed_targets
                } else {
                    applied_targets(&mut tx, &cs).await?
                };
                mark_reviewed(
                    &mut tx,
                    id,
                    "applied",
                    actor,
                    note.as_deref(),
                    Some((&target_type, &target_id)),
                    now,
                )
                .await?;
                sqlx::query("UPDATE change_sets SET published_by=$2,published_at=$3,reviewed_by=CASE WHEN $4 THEN $5 ELSE NULL END,reviewed_at=CASE WHEN $4 THEN $6 ELSE NULL END WHERE id=$1")
                    .bind(id).bind(actor).bind(now).bind(publishing).bind(cs.reviewed_by).bind(cs.reviewed_at).execute(&mut *tx).await.map_err(storage_error)?;
                insert_event(
                    &mut tx,
                    id,
                    "published",
                    "user",
                    Some(actor),
                    json!({}),
                    now,
                )
                .await?;
                insert_event(
                    &mut tx,
                    id,
                    "applied",
                    "user",
                    Some(actor),
                    json!({"targetType":target_type,"targetID":target_id,"appliedTargets":targets}),
                    now,
                )
                .await?;
            }
            _ => {
                return Err(conflict(
                    "STATE_CHANGED",
                    "this is not a lifecycle transition",
                ));
            }
        }
        tx.commit().await.map_err(storage_error)?;
        required_detail(&self.pool, actor, id).await
    }
}
pub(crate) async fn required_detail(
    pool: &PgPool,
    actor: i64,
    id: i64,
) -> Result<package::ChangeSetDetail, gql::ChangeSetError> {
    detail(
        pool,
        package::PackageViewer {
            actor_user_id: actor,
            can_write: true,
        },
        id,
    )
    .await?
    .ok_or_else(|| conflict("STATE_CHANGED", "package disappeared"))
}
struct ProviderInput {
    key: String,
    currency: String,
    after: Value,
    error: Option<String>,
}
#[derive(Clone)]
pub(crate) struct Proposal {
    pub key: String,
    pub action: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub source: Value,
    pub error: Option<String>,
}
fn prepare_provider(
    input: Vec<conduit_admin_graphql::model_ext::SaveChannelModelPriceInput>,
) -> Result<Vec<ProviderInput>, gql::ChangeSetError> {
    let mut values = BTreeMap::new();
    for v in input {
        let key = v.model_id.trim().to_string();
        if key.is_empty() || values.contains_key(&key) {
            return Err(conflict(
                "VALIDATION_FAILED",
                "model keys must be nonempty and unique",
            ));
        }
        let after = match (v.price, v.billing_expression) {
            (Some(price), None) => {
                serde_json::to_value(crate::conv::model_price_input_to_core(price))
                    .map_err(storage_error)?
            }
            (None, Some(expression)) => {
                let price: conduit_core::objects::newapi_price::NewApiPrice =
                    serde_json::from_value(expression.0.clone()).map_err(|error| {
                        conflict(
                            "VALIDATION_FAILED",
                            format!("invalid billingExpression: {error}"),
                        )
                    })?;
                price
                    .compile()
                    .map_err(|error| conflict("VALIDATION_FAILED", error))?;
                expression.0
            }
            _ => {
                return Err(conflict(
                    "VALIDATION_FAILED",
                    "exactly one of price and billingExpression is required",
                ));
            }
        };
        let error = validate_price_for_type(&after, conduit_llm::RequestType::Chat).err();
        values.insert(
            key.clone(),
            ProviderInput {
                key,
                currency: v.currency_code.trim().to_ascii_uppercase(),
                after,
                error,
            },
        );
    }
    Ok(values.into_values().collect())
}

/// The original manual-price mutation writes immediately when review is off.
/// Its validation, audit and price-version receipt use the same transaction
/// applier as an explicitly published package.
pub(crate) async fn save_native_provider(
    pool: &PgPool,
    actor: i64,
    channel: i64,
    input: Vec<conduit_admin_graphql::model_ext::SaveChannelModelPriceInput>,
) -> Result<(), String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| "price transaction unavailable")?;
    let policy = crate::wiring_governance::lock_policy(&mut tx).await?;
    crate::wiring_governance::authorize(&mut tx, actor, "write_commercialization").await?;
    crate::wiring::lock_accounting_currency_price_writes(&mut tx)
        .await
        .map_err(|_| "price settings lock failed")?;
    let base = provider_base_in_tx(&mut tx, channel)
        .await
        .map_err(|_| "price base unavailable")?;
    let existing =
        conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(&mut tx, channel)
            .await
            .map_err(|_| "price list unavailable")?;
    let prepared = prepare_provider(input).map_err(|_| "invalid price input")?;
    let proposals = provider_changes(
        &existing,
        &prepared,
        &base.accounting_currency,
        &base.accounting_settings_version,
        None,
    )
    .map_err(|_| "invalid price input")?;
    if proposals.iter().any(|p| p.error.is_some()) {
        return Err("VALIDATION_FAILED: invalid procurement price".into());
    }
    if proposals.is_empty() {
        tx.commit().await.map_err(|_| "price commit failed")?;
        return Ok(());
    }
    let now = Utc::now();
    let id:i64=sqlx::query_scalar("INSERT INTO change_sets(operation_id,kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,created_at,updated_at) VALUES($1,'provider_price','channel',$2,'Manual procurement price edit',$3,$4,$5,$6,$7,$7) RETURNING id").bind(Uuid::new_v4()).bind(channel.to_string()).bind(if policy.requires("provider_price"){"draft"}else{"pending_review"}).bind(base.revision).bind(base.accounting_settings_version).bind(actor).bind(now).fetch_one(&mut *tx).await.map_err(|_|"price proposal failed")?;
    write_proposals(&mut tx, id, &proposals, now)
        .await
        .map_err(|_| "price proposal failed")?;
    insert_event(
        &mut tx,
        id,
        "created",
        "user",
        Some(actor),
        json!({"origin":"MANUAL_EDIT","itemCount":proposals.len()}),
        now,
    )
    .await
    .map_err(|_| "price audit failed")?;
    if !policy.requires("provider_price") {
        let cs = lock_change_set(&mut tx, id)
            .await
            .map_err(|_| "price package unavailable")?;
        sqlx::query("SELECT set_config('conduit.governance_publish',$1,true)")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|_| "price publication authority unavailable")?;
        let (kind, target) = apply_provider_price(&mut tx, &cs, actor)
            .await
            .map_err(|_| "VALIDATION_FAILED: price publication rejected")?;
        sqlx::query("UPDATE change_sets SET status='applied',applied_at=$2,published_at=$2,published_by=$3,applied_target_type=$4,applied_target_id=$5,updated_at=$2 WHERE id=$1").bind(id).bind(now).bind(actor).bind(&kind).bind(&target).execute(&mut *tx).await.map_err(|_|"price receipt failed")?;
        insert_event(
            &mut tx,
            id,
            "published",
            "user",
            Some(actor),
            json!({"policyRevision":policy.revision,"approvalRequired":false}),
            now,
        )
        .await
        .map_err(|_| "price audit failed")?;
        insert_event(
            &mut tx,
            id,
            "applied",
            "user",
            Some(actor),
            json!({"targetType":kind,"targetID":target}),
            now,
        )
        .await
        .map_err(|_| "price audit failed")?;
    }
    tx.commit().await.map_err(|_| "price commit failed")?;
    Ok(())
}
fn provider_changes(
    existing: &[conduit_db::row::ChannelModelPriceRow],
    prepared: &[ProviderInput],
    currency: &str,
    version: &str,
    correction: Option<Value>,
) -> Result<Vec<Proposal>, gql::ChangeSetError> {
    let version: u64 = version.parse().map_err(storage_error)?;
    let source = json!({"source":"manual","accountingCurrency":currency,"accountingSettingsVersion":version,"correction":correction});
    let mut proposals = Vec::new();
    for v in prepared {
        let before = existing
            .iter()
            .find(|p| p.model_id == v.key)
            .map(base_price_snapshot);
        if existing
            .iter()
            .find(|p| p.model_id == v.key)
            .is_some_and(|p| p.price == v.after && p.currency_code == v.currency)
        {
            continue;
        }
        let error = v.error.clone().or_else(|| {
            let expected = if v.after.get("kind").is_some() {
                "USD"
            } else {
                currency
            };
            if v.currency != expected {
                Some(format!("currencyCode must be {expected}"))
            } else {
                None
            }
        });
        proposals.push(Proposal {
            key: v.key.clone(),
            action: if before.is_some() { "update" } else { "create" }.into(),
            before,
            after: Some(v.after.clone()),
            source: source.clone(),
            error,
        });
    }
    for old in existing
        .iter()
        .filter(|p| !prepared.iter().any(|v| v.key == p.model_id))
    {
        proposals.push(Proposal {
            key: old.model_id.clone(),
            action: "delete".into(),
            before: Some(base_price_snapshot(old)),
            after: None,
            source: source.clone(),
            error: None,
        });
    }
    proposals.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(proposals)
}
fn contains_expression_price(value: &Value) -> bool {
    conduit_core::objects::retail_price_plan::RetailPriceDocument::parse(value).is_ok_and(|doc| {
        doc.leaves()
            .iter()
            .any(|leaf| leaf.static_price().is_none())
    })
}

fn validate_price(value: &Value) -> Result<(), String> {
    let document = conduit_core::objects::retail_price_plan::RetailPriceDocument::parse(value)?;
    for price in document
        .leaves()
        .into_iter()
        .filter_map(|leaf| leaf.static_price())
    {
        crate::wiring::validate_model_price(price)?;
    }
    Ok(())
}
fn price_request_type(kind: &str) -> conduit_llm::RequestType {
    use conduit_llm::RequestType;
    match kind {
        "embedding" => RequestType::Embedding,
        "rerank" => RequestType::Rerank,
        "image_generation" => RequestType::Image,
        "video_generation" => RequestType::Video,
        _ => RequestType::Chat,
    }
}
fn validate_price_for_type(value: &Value, kind: conduit_llm::RequestType) -> Result<(), String> {
    validate_price(value)?;
    let document = conduit_core::objects::retail_price_plan::RetailPriceDocument::parse(value)?;
    for price in document
        .leaves()
        .into_iter()
        .filter_map(|leaf| leaf.static_price())
    {
        crate::wiring_postgres_pricing_admission::validate_price_coverage(price, kind)?;
    }
    Ok(())
}

pub(crate) async fn write_proposals(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    proposals: &[Proposal],
    now: DateTime<Utc>,
) -> Result<(), gql::ChangeSetError> {
    for p in proposals {
        sqlx::query("INSERT INTO change_set_items(change_set_id,item_key,action,before_snapshot,after_snapshot,source_snapshot,validation_error,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$8)").bind(id).bind(&p.key).bind(&p.action).bind(p.before.clone().map(SqlJson)).bind(p.after.clone().map(SqlJson)).bind(SqlJson(p.source.clone())).bind(&p.error).bind(now).execute(&mut **tx).await.map_err(storage_error)?;
    }
    Ok(())
}
async fn mapping_current(
    pool: &PgPool,
    item: &ChangeSetItemRow,
) -> Result<Option<Value>, gql::ChangeSetError> {
    let Some(after) = item.after_snapshot.as_ref().map(|v| &v.0) else {
        return Ok(None);
    };
    let Some(key) = after
        .pointer("/publicModel/modelID")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let deployment = after
        .get("deploymentID")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let row=sqlx::query("SELECT m.id,m.status,m.developer,m.\"type\",m.name,m.\"group\",r.status AS route_status FROM models m LEFT JOIN model_routes r ON r.public_model_id=m.id AND r.deployment_id=$2 WHERE m.model_id=$1 AND m.deleted_at=0").bind(key).bind(deployment).fetch_optional(pool).await.map_err(query_error)?;
    Ok(row.map(|r|json!({"id":r.get::<i64,_>("id"),"status":r.get::<String,_>("status"),"developer":r.get::<String,_>("developer"),"type":r.get::<String,_>("type"),"name":r.get::<String,_>("name"),"group":r.get::<String,_>("group"),"routeStatus":r.get::<Option<String>,_>("route_status")})))
}
async fn validate_items(
    tx: &mut Transaction<'_, Postgres>,
    cs: &ChangeSetRow,
) -> Result<Vec<Value>, gql::ChangeSetError> {
    if cs.scope_type == "governed" {
        return match crate::wiring_governance_commands::check(tx, cs.id, cs.created_by).await {
            Ok(()) => Ok(vec![]),
            Err(error) => Ok(vec![
                json!({"itemKey":"command","code":if error=="SOURCE_CHANGED" {"SOURCE_CHANGED"}else{"VALIDATION_FAILED"},"message":"governed command validation failed"}),
            ]),
        };
    }
    let items = load_items_in_tx(tx, cs.id)
        .await
        .map_err(|e| gql::ChangeSetError::Storage(format!("{e:?}")))?;
    let mut issues = Vec::new();
    if items.is_empty() {
        issues.push(json!({"code":"EMPTY_PACKAGE","message":"package must contain at least one actual change"}));
        return Ok(issues);
    }
    crate::wiring::lock_accounting_currency_price_writes(tx)
        .await
        .map_err(storage_error)?;
    let settings=crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(tx).await.map_err(storage_error)?;
    let scope = parse_numeric_id(&cs.scope_id, "scope")?;
    let mut shared_error: Option<String> = None;
    let mut current_prices = BTreeMap::new();
    let mut channel_settings = None;
    let mut retail_reference = String::new();
    if cs.kind == "provider_price" {
        let raw = sqlx::query_scalar::<_, SqlJson<Value>>(
            "SELECT settings FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(scope)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage_error)?;
        if let Some(raw) = raw {
            channel_settings = Some(
                serde_json::from_value::<conduit_core::objects::channel_settings::ChannelSettings>(
                    raw.0,
                )
                .map_err(storage_error)?,
            );
        } else {
            shared_error = Some("SOURCE_CHANGED: channel is unavailable".into());
        }
        current_prices =
            conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(tx, scope)
                .await
                .map_err(storage_error)?
                .into_iter()
                .map(|r| (r.model_id.clone(), base_price_snapshot(&r)))
                .collect();
        let observed = items.iter().any(|i| {
            i.source_snapshot
                .as_ref()
                .and_then(|v| v.0.get("source"))
                .and_then(Value::as_str)
                != Some("manual")
        });
        if observed {
            let latest=sqlx::query_scalar::<_,i64>("SELECT id FROM provider_price_snapshots WHERE channel_id=$1 AND status='success' ORDER BY observed_at DESC,id DESC LIMIT 1").bind(scope).fetch_optional(&mut **tx).await.map_err(storage_error)?;
            if latest.map(|id| id.to_string()).as_deref() != Some(cs.source_revision.as_str()) {
                shared_error =
                    Some("SOURCE_CHANGED: provider observation is no longer current".into());
            }
        }
    } else if cs.kind == "retail_price" {
        let currency = sqlx::query_scalar::<_, String>(
            "SELECT currency FROM price_books WHERE id=$1 FOR UPDATE",
        )
        .bind(scope)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage_error)?;
        if currency
            .as_deref()
            .is_none_or(|c| !c.eq_ignore_ascii_case(&settings.currency))
        {
            shared_error =
                Some("VALIDATION_FAILED: price book accounting currency is invalid".into());
        }
        retail_reference=sqlx::query_scalar::<_,String>("SELECT reference_id FROM price_book_versions WHERE price_book_id=$1 AND status='published' ORDER BY version DESC LIMIT 1").bind(scope).fetch_optional(&mut **tx).await.map_err(storage_error)?.unwrap_or_default();
        if retail_reference != cs.base_revision
            || cs.source_revision != settings.version.to_string()
        {
            shared_error =
                Some("SOURCE_CHANGED: published retail base or accounting settings changed".into());
        }
    }
    let mut mapping_metadata = BTreeMap::<String, Value>::new();
    if cs.kind == "model_mapping" {
        let mut keys = items
            .iter()
            .filter_map(|i| {
                i.after_snapshot
                    .as_ref()?
                    .0
                    .pointer("/publicModel/modelID")?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        for key in keys {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::BIGINT)")
                .bind(&key)
                .execute(&mut **tx)
                .await
                .map_err(storage_error)?;
        }
    }
    for item in items {
        let shape = if item.action == "delete" {
            if item.before_snapshot.is_none() || item.after_snapshot.is_some() {
                Some("DELETE requires before and no proposed value".into())
            } else if cs.kind != "provider_price" {
                Some("DELETE is not supported for this package kind".into())
            } else {
                None
            }
        } else if !matches!(item.action.as_str(), "create" | "update") {
            Some("unknown item action".into())
        } else if item.action == "update" && item.before_snapshot.is_none() {
            Some("UPDATE requires an original snapshot".into())
        } else if let Some(v) = &item.after_snapshot {
            match cs.kind.as_str() {
                "provider_price" => {
                    validate_price_for_type(&v.0, conduit_llm::RequestType::Chat).err()
                }
                "retail_price" => validate_price(&v.0).err(),
                "model_mapping" => validate_mapping_shape(&v.0).err(),
                _ => Some("unknown package kind".into()),
            }
        } else {
            Some("proposed value is missing".into())
        };
        let mut error = shape
            .map(|m| format!("VALIDATION_FAILED: {m}"))
            .or_else(|| shared_error.clone());
        let source = item.source_snapshot.as_ref().map(|v| &v.0);
        if cs.kind == "provider_price" {
            if current_prices.get(&item.item_key) != item.before_snapshot.as_ref().map(|v| &v.0) {
                error = Some("SOURCE_CHANGED: formal procurement price changed".into());
            }
            if let Some(source) = source {
                if source
                    .get("accountingCurrency")
                    .and_then(Value::as_str)
                    .is_none_or(|c| !c.eq_ignore_ascii_case(&settings.currency))
                {
                    error = Some(
                        "VALIDATION_FAILED: accounting currency is missing or mismatched".into(),
                    );
                }
                if source
                    .get("accountingSettingsVersion")
                    .and_then(Value::as_u64)
                    != Some(settings.version)
                {
                    error = Some("SOURCE_CHANGED: accounting settings version changed".into());
                }
                if source.get("source").and_then(Value::as_str) != Some("manual") {
                    if let Some(c) = &channel_settings {
                        if let Err(m)=crate::wiring_postgres_pricing_admission::validate_channel_billing_snapshot(source,&c.billing_currency,c.recharge_multiplier){error=Some(format!("SOURCE_CHANGED: {m}"));}
                    }
                    let snapshot = source.get("snapshotID").and_then(Value::as_i64);
                    let row = source.get("providerPriceRowID").and_then(Value::as_i64);
                    let proof=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM provider_price_rows r JOIN provider_price_snapshots s ON s.id=r.snapshot_id WHERE r.id=$1 AND r.snapshot_id=$2 AND r.channel_id=$3 AND s.channel_id=$3 AND r.upstream_model_id=$4 AND s.status='success')").bind(row).bind(snapshot).bind(scope).bind(&item.item_key).fetch_one(&mut **tx).await.map_err(storage_error)?;
                    if !proof
                        || snapshot.map(|n| n.to_string()).as_deref()
                            != Some(cs.source_revision.as_str())
                    {
                        error=Some("VALIDATION_FAILED: provider source row lineage is missing or mismatched".into());
                    }
                    error = error.or_else(|| item.validation_error.clone());
                }
            } else {
                error = Some("VALIDATION_FAILED: provider source is missing".into());
            }
        } else if cs.kind == "retail_price" {
            if let Ok(model) = parse_numeric_id(&item.item_key, "public model") {
                let model_type = sqlx::query_scalar::<_, String>(
                    "SELECT \"type\" FROM models WHERE id=$1 AND deleted_at=0",
                )
                .bind(model)
                .fetch_optional(&mut **tx)
                .await
                .map_err(storage_error)?;
                if item
                    .after_snapshot
                    .as_ref()
                    .is_some_and(|after| contains_expression_price(&after.0))
                    && let Err(message) =
                        crate::wiring_postgres_pricing_admission::expression_retail_route_warnings(
                            tx, model,
                        )
                        .await
                {
                    error = Some(message);
                }
                if let Some(kind) = model_type {
                    if let Some(after) = item.after_snapshot.as_ref() {
                        if let Err(message) =
                            validate_price_for_type(&after.0, price_request_type(&kind))
                        {
                            error = Some(format!("VALIDATION_FAILED: {message}"));
                        }
                    }
                } else {
                    error = Some("SOURCE_CHANGED: public model is unavailable".into());
                }
                let current=sqlx::query_scalar::<_,SqlJson<Value>>("SELECT i.price FROM price_book_items i JOIN price_book_versions v ON v.id=i.price_book_version_id WHERE v.price_book_id=$1 AND v.reference_id=$2 AND i.public_model_id=$3").bind(scope).bind(&retail_reference).bind(model).fetch_optional(&mut **tx).await.map_err(storage_error)?;
                if current.as_ref().map(|v| &v.0) != item.before_snapshot.as_ref().map(|v| &v.0) {
                    error = Some("SOURCE_CHANGED: retail original price changed".into());
                }
                if let (Some(source), Some(proposed)) = (source, item.after_snapshot.as_ref()) {
                    if let Err(message) = crate::wiring_postgres_price_formula::validate_derived(
                        tx,
                        scope,
                        model,
                        source,
                        &proposed.0,
                    )
                    .await
                    {
                        error = Some(format!("SOURCE_CHANGED: {message}"));
                    }
                }
            } else {
                error = Some("VALIDATION_FAILED: public model ID is invalid".into());
            }
        } else if cs.kind == "model_mapping" {
            if let Some(after) = item.after_snapshot.as_ref().map(|v| &v.0) {
                let deployment = if let Some(upstream) =
                    after.get("upstreamModelID").and_then(Value::as_str)
                {
                    sqlx::query_scalar::<_,i64>("SELECT id FROM upstream_model_deployments WHERE channel_id=$1 AND upstream_model_id=$2 AND variant=''").bind(scope).bind(upstream).fetch_optional(&mut **tx).await.map_err(storage_error)?
                } else {
                    after.get("deploymentID").and_then(Value::as_i64)
                };
                let public = &after["publicModel"];
                let key = public["modelID"].as_str().unwrap_or_default();
                if mapping_metadata.get(key).is_some_and(|v| v != public) {
                    error = Some(
                        "VALIDATION_FAILED: mappings disagree about public model metadata".into(),
                    );
                }
                mapping_metadata.insert(key.into(), public.clone());
                let available = if let Some(upstream) =
                    after.get("upstreamModelID").and_then(Value::as_str)
                {
                    sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM channels WHERE id=$1 AND deleted_at=0 AND supported_models ? $2)").bind(scope).bind(upstream).fetch_one(&mut **tx).await.map_err(storage_error)?
                } else {
                    sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM upstream_model_deployments WHERE id=$1 AND channel_id=$2 AND status='enabled')").bind(deployment).bind(scope).fetch_one(&mut **tx).await.map_err(storage_error)?
                };
                if !available {
                    error = Some("SOURCE_CHANGED: upstream deployment is unavailable".into());
                }
                let existing=sqlx::query("SELECT id,status,developer,\"type\",name,\"group\" FROM models WHERE model_id=$1 AND deleted_at=0").bind(key).fetch_optional(&mut **tx).await.map_err(storage_error)?;
                let current = if let Some(row) = existing {
                    let model: i64 = row.get("id");
                    if row.get::<String, _>("status") == "archived" {
                        error = Some("VALIDATION_FAILED: public model is archived".into());
                    }
                    let route=sqlx::query_scalar::<_,String>("SELECT status FROM model_routes WHERE public_model_id=$1 AND deployment_id=$2").bind(model).bind(deployment).fetch_optional(&mut **tx).await.map_err(storage_error)?;
                    Some(
                        json!({"id":model,"status":row.get::<String,_>("status"),"developer":row.get::<String,_>("developer"),"type":row.get::<String,_>("type"),"name":row.get::<String,_>("name"),"group":row.get::<String,_>("group"),"routeStatus":route}),
                    )
                } else {
                    None
                };
                if current != item.before_snapshot.as_ref().map(|v| v.0.clone()) {
                    error = Some("SOURCE_CHANGED: public model or route changed".into());
                }
            }
        }
        error = error.or_else(|| item.validation_error.clone());
        sqlx::query("UPDATE change_set_items SET validation_error=$2 WHERE id=$1")
            .bind(item.id)
            .bind(&error)
            .execute(&mut **tx)
            .await
            .map_err(storage_error)?;
        if let Some(message) = error {
            let code = if message.starts_with("SOURCE_CHANGED:") {
                "SOURCE_CHANGED"
            } else {
                "VALIDATION_FAILED"
            };
            issues.push(json!({"itemKey":item.item_key,"code":code,"message":message}));
        }
    }
    Ok(issues)
}
fn validate_mapping_shape(value: &Value) -> Result<(), String> {
    if value.get("deploymentID").and_then(Value::as_i64).is_none()
        && value
            .get("upstreamModelID")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err("deploymentID or observed upstreamModelID is required".into());
    }
    let model = value
        .get("publicModel")
        .and_then(Value::as_object)
        .ok_or("publicModel is required")?;
    for key in ["modelID", "developer", "type", "name", "group"] {
        if model
            .get(key)
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
        {
            return Err(format!("publicModel.{key} is required"));
        }
    }
    Ok(())
}
async fn persist_failure_issues(
    tx: &mut Transaction<'_, Postgres>,
    cs: &ChangeSetRow,
    message: &str,
) -> Result<(), gql::ChangeSetError> {
    // No guessed field name: preserve the domain applier's diagnosis on the matching item,
    // or the package if the failure cannot be attributed reliably.
    let keys = sqlx::query_as::<_, (i64, String)>(
        "SELECT id,item_key FROM change_set_items WHERE change_set_id=$1 ORDER BY id",
    )
    .bind(cs.id)
    .fetch_all(&mut **tx)
    .await
    .map_err(storage_error)?;
    for (id, key) in keys {
        if message.starts_with(&format!("{key}:")) {
            sqlx::query("UPDATE change_set_items SET validation_error=$2 WHERE id=$1")
                .bind(id)
                .bind(message)
                .execute(&mut **tx)
                .await
                .map_err(storage_error)?;
        }
    }
    Ok(())
}
async fn applied_targets(
    tx: &mut Transaction<'_, Postgres>,
    cs: &ChangeSetRow,
) -> Result<Vec<Value>, gql::ChangeSetError> {
    let items = load_items_in_tx(tx, cs.id)
        .await
        .map_err(|e| gql::ChangeSetError::Storage(format!("{e:?}")))?;
    let mut result = Vec::new();
    for item in items {
        let (kind, id) = match cs.kind.as_str() {
            "model_mapping" => {
                let after = item
                    .after_snapshot
                    .as_ref()
                    .map(|v| &v.0)
                    .ok_or_else(|| conflict("VALIDATION_FAILED", "mapping missing"))?;
                let id=sqlx::query_scalar::<_,i64>("SELECT r.id FROM model_routes r JOIN models m ON m.id=r.public_model_id JOIN upstream_model_deployments d ON d.id=r.deployment_id WHERE m.model_id=$1 AND m.deleted_at=0 AND (r.deployment_id=$2 OR (d.channel_id::text=$3 AND d.upstream_model_id=$4 AND d.variant=''))").bind(after.pointer("/publicModel/modelID").and_then(Value::as_str)).bind(after.get("deploymentID").and_then(Value::as_i64)).bind(&cs.scope_id).bind(after.get("upstreamModelID").and_then(Value::as_str)).fetch_one(&mut **tx).await.map_err(storage_error)?;
                ("model_route", id)
            }
            "provider_price" => {
                let id = if item.action == "delete" {
                    item.before_snapshot
                        .as_ref()
                        .and_then(|v| v.0["id"].as_i64())
                        .ok_or_else(|| {
                            conflict("VALIDATION_FAILED", "deleted price identity is missing")
                        })?
                } else {
                    sqlx::query_scalar::<_,i64>("SELECT id FROM channel_model_prices WHERE channel_id=$1 AND model_id=$2 AND deleted_at=0").bind(parse_numeric_id(&cs.scope_id,"channel")?).bind(&item.item_key).fetch_one(&mut **tx).await.map_err(storage_error)?
                };
                ("channel_price", id)
            }
            "retail_price" => {
                let id=sqlx::query_scalar::<_,i64>("SELECT i.id FROM price_book_items i JOIN price_book_versions v ON v.id=i.price_book_version_id WHERE v.price_book_id=$1 AND v.status='published' AND i.public_model_id=$2").bind(parse_numeric_id(&cs.scope_id,"book")?).bind(parse_numeric_id(&item.item_key,"model")?).fetch_one(&mut **tx).await.map_err(storage_error)?;
                ("retail_price", id)
            }
            _ => return Err(conflict("VALIDATION_FAILED", "unknown target kind")),
        };
        result.push(json!({"type":kind,"id":id.to_string(),"itemKey":item.item_key}));
    }
    Ok(result)
}

#[derive(Debug, Clone)]
pub(crate) struct StagedPackage {
    pub id: i64,
    pub reused: bool,
    pub item_count: usize,
    pub invalid_count: usize,
}
fn semantic_source(mut source: Value) -> Value {
    if let Some(m) = source.as_object_mut() {
        for key in [
            "snapshotID",
            "providerPriceRowID",
            "fetchedAt",
            "observedAt",
            "sourceRevision",
        ] {
            m.remove(key);
        }
    }
    source
}
fn proposal_fingerprint(items: &[Proposal]) -> String {
    let mut values=items.iter().map(|p|json!({"key":p.key,"action":p.action,"before":p.before,"after":p.after,"source":semantic_source(p.source.clone()),"error":p.error})).collect::<Vec<_>>();
    values.sort_by_key(|v| v["key"].as_str().unwrap_or_default().to_string());
    digest(&json!(values))
}
/// Whole observations are the dedupe/supersession unit. Human packages are never implicitly superseded.
pub(crate) async fn stage_observed(
    tx: &mut Transaction<'_, Postgres>,
    context: &OperationContext,
    kind: &str,
    channel: i64,
    title: &str,
    source_revision: &str,
    proposals: &[Proposal],
    now: DateTime<Utc>,
) -> Result<Option<StagedPackage>, String> {
    let policy = crate::wiring_governance::lock_policy(tx).await?;
    let requires_review =
        policy.requires(kind) || (kind == "model_mapping" && policy.requires("model_supply"));
    // A successful observation does not confer authority to publish its side writes.
    // Human receipts keep their actor even when a background phase completes them.
    let may_publish = match context.actor_user_id {
        Some(actor) => {
            crate::wiring_governance::authorize(tx, actor, "write_channels")
                .await
                .is_ok()
                && crate::wiring_governance::authorize(tx, actor, "write_commercialization")
                    .await
                    .is_ok()
        }
        None if context.origin
            == conduit_admin_graphql::change_package::OperationOrigin::AutomaticSync =>
        {
            true
        }
        None => return Err("FORBIDDEN: observation producer identity is missing".into()),
    };
    let fp = proposal_fingerprint(proposals);
    let origin = context.origin.as_str();
    let mut automatic = Vec::new();
    if context.origin == package::OperationOrigin::AutomaticSync {
        let candidates=sqlx::query_as::<_,(i64,Option<i64>,String,Option<String>)>("SELECT cs.id,cs.created_by,cs.source_revision,(SELECT detail->>'origin' FROM change_set_events WHERE change_set_id=cs.id AND detail ? 'origin' ORDER BY id LIMIT 1) AS origin FROM change_sets cs WHERE kind=$1 AND scope_type='channel' AND scope_id=$2 AND status IN('pending_review','invalid') ORDER BY id FOR UPDATE").bind(kind).bind(channel.to_string()).fetch_all(&mut **tx).await.map_err(|e|e.to_string())?;
        for (id, actor, revision, recorded_origin) in candidates {
            let items = load_items_in_tx(tx, id)
                .await
                .map_err(|e| format!("{e:?}"))?;
            let proved = if recorded_origin.as_deref() == Some("AUTOMATIC_SYNC") {
                true
            } else if recorded_origin.is_none() && actor.is_none() && !items.is_empty() {
                let created_system=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM change_set_events WHERE change_set_id=$1 AND actor_type='system' AND actor_id IS NULL)").bind(id).fetch_one(&mut **tx).await.map_err(|e|e.to_string())?;
                let mut proof = created_system;
                for item in &items {
                    let source = item.source_snapshot.as_ref().map(|v| &v.0);
                    if kind == "provider_price" {
                        let snapshot = source
                            .and_then(|s| s.get("snapshotID"))
                            .and_then(Value::as_i64);
                        let row = source
                            .and_then(|s| s.get("providerPriceRowID"))
                            .and_then(Value::as_i64);
                        let valid = if let (Some(snapshot), Some(row)) = (snapshot, row) {
                            revision==snapshot.to_string()&&sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM provider_price_rows r JOIN provider_price_snapshots s ON s.id=r.snapshot_id WHERE r.id=$1 AND r.snapshot_id=$2 AND r.channel_id=$3 AND s.channel_id=$3 AND r.upstream_model_id=$4 AND s.status='success')").bind(row).bind(snapshot).bind(channel).bind(&item.item_key).fetch_one(&mut **tx).await.map_err(|e|e.to_string())?
                        } else {
                            false
                        };
                        proof &= valid;
                    } else {
                        proof &= source
                            .and_then(|s| s.get("channelID"))
                            .and_then(Value::as_i64)
                            == Some(channel)
                            && source.and_then(|s| s.get("source")).and_then(Value::as_str)
                                != Some("manual");
                    }
                }
                proof
            } else {
                false
            };
            if proved {
                let old = items
                    .into_iter()
                    .map(|i| Proposal {
                        key: i.item_key,
                        action: i.action,
                        before: i.before_snapshot.map(|v| v.0),
                        after: i.after_snapshot.map(|v| v.0),
                        source: i.source_snapshot.map(|v| v.0).unwrap_or(Value::Null),
                        error: i.validation_error,
                    })
                    .collect::<Vec<_>>();
                automatic.push((id, proposal_fingerprint(&old)));
            }
        }
        if requires_review && !proposals.is_empty() {
            if let Some((id, _)) = automatic.iter().find(|(_, old)| old == &fp) {
                for p in proposals {
                    sqlx::query("UPDATE change_set_items SET source_snapshot=$3,updated_at=$4 WHERE change_set_id=$1 AND item_key=$2").bind(id).bind(&p.key).bind(SqlJson(p.source.clone())).bind(now).execute(&mut **tx).await.map_err(|e|e.to_string())?;
                }
                sqlx::query("UPDATE change_sets SET source_revision=$2,updated_at=$3 WHERE id=$1")
                    .bind(id)
                    .bind(source_revision)
                    .bind(now)
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| e.to_string())?;
                insert_event(
                    tx,
                    *id,
                    "source_refreshed",
                    "system",
                    None,
                    json!({"sourceRevision":source_revision,"semanticFingerprint":fp}),
                    now,
                )
                .await
                .map_err(|e| e.to_string())?;
                for (other, _) in automatic.iter().filter(|(other, _)| other != id) {
                    supersede_observed(tx, *other, Some(*id), context, now).await?;
                }
                return Ok(Some(StagedPackage {
                    id: *id,
                    reused: true,
                    item_count: proposals.len(),
                    invalid_count: proposals.iter().filter(|p| p.error.is_some()).count(),
                }));
            }
        }
    }
    if proposals.is_empty() {
        for (id, _) in automatic {
            supersede_observed(tx, id, None, context, now).await?;
        }
        return Ok(None);
    }
    if let Some(id)=sqlx::query_scalar::<_,i64>("SELECT id FROM change_sets WHERE operation_id=$1 AND kind=$2 AND scope_type='channel' AND scope_id=$3").bind(context.operation_id).bind(kind).bind(channel.to_string()).fetch_optional(&mut **tx).await.map_err(|e|e.to_string())?{
        let recorded=sqlx::query_scalar::<_,Option<String>>("SELECT detail->>'semanticFingerprint' FROM change_set_events WHERE change_set_id=$1 AND event_type='created' ORDER BY id LIMIT 1").bind(id).fetch_optional(&mut **tx).await.map_err(|e|e.to_string())?.flatten();if recorded.as_deref()!=Some(fp.as_str()){return Err("OPERATION_CONFLICT: an operation cannot replace its package proposal".into());}return Ok(Some(StagedPackage{id,reused:true,item_count:proposals.len(),invalid_count:proposals.iter().filter(|p|p.error.is_some()).count()}));
    }
    let invalid_count = proposals.iter().filter(|p| p.error.is_some()).count();
    let error =
        (invalid_count > 0).then(|| format!("{invalid_count} package item(s) require correction"));
    let status = if error.is_some() {
        "invalid"
    } else {
        "pending_review"
    };
    let id=sqlx::query_scalar::<_,i64>("INSERT INTO change_sets(operation_id,kind,scope_type,scope_id,title,status,source_revision,validation_error,created_by,submitted_by,submitted_at,created_at,updated_at) VALUES($1,$2,'channel',$3,$4,$5,$6,$7,$8,$8,CASE WHEN $5='pending_review' THEN $9 ELSE NULL END,$9,$9) RETURNING id").bind(context.operation_id).bind(kind).bind(channel.to_string()).bind(title).bind(status).bind(source_revision).bind(error).bind(context.actor_user_id).bind(now).fetch_one(&mut **tx).await.map_err(|e|e.to_string())?;
    write_proposals(tx, id, proposals, now)
        .await
        .map_err(|e| e.to_string())?;
    insert_event(tx,id,"created",if context.actor_user_id.is_some(){"user"}else{"system"},context.actor_user_id,json!({"origin":origin,"operationID":context.operation_id,"semanticFingerprint":fp,"itemCount":proposals.len()}),now).await.map_err(|e|e.to_string())?;
    insert_event(
        tx,
        id,
        if invalid_count > 0 {
            "validation_failed"
        } else {
            "submitted"
        },
        if context.actor_user_id.is_some() {
            "user"
        } else {
            "system"
        },
        context.actor_user_id,
        json!({"invalidItemCount":invalid_count}),
        now,
    )
    .await
    .map_err(|e| e.to_string())?;
    for (old, _) in automatic {
        supersede_observed(tx, old, Some(id), context, now).await?;
    }
    if !requires_review && invalid_count == 0 && !may_publish {
        insert_event(
            tx,
            id,
            "publication_blocked",
            "user",
            context.actor_user_id,
            json!({"code":"FORBIDDEN","reason":"current_side_write_authority_required"}),
            now,
        )
        .await
        .map_err(|_| "observation authority receipt failed")?;
    }
    if !requires_review && invalid_count == 0 && may_publish {
        let cs = lock_change_set(tx, id)
            .await
            .map_err(|_| "observed package lookup failed")?;
        sqlx::query("SELECT set_config('conduit.governance_publish',$1,true)")
            .bind(id.to_string())
            .execute(&mut **tx)
            .await
            .map_err(|_| "publication authority unavailable")?;
        let actor = context.actor_user_id.unwrap_or(0);
        let result = match kind {
            "provider_price" => apply_provider_price(tx, &cs, actor).await,
            "model_mapping" => apply_model_mapping(tx, &cs).await,
            _ => return Err("unsupported observed publication".into()),
        };
        let (target_type, target_id) =
            result.map_err(|_| "observed configuration publication failed")?;
        sqlx::query("UPDATE change_sets SET status='applied',applied_at=$2,published_at=$2,published_by=$3,applied_target_type=$4,applied_target_id=$5,updated_at=$2 WHERE id=$1").bind(id).bind(now).bind(context.actor_user_id).bind(&target_type).bind(&target_id).execute(&mut **tx).await.map_err(|_|"observed publication receipt failed")?;
        insert_event(
            tx,
            id,
            "published",
            if context.actor_user_id.is_some() {
                "user"
            } else {
                "system"
            },
            context.actor_user_id,
            json!({"policyRevision":policy.revision,"approvalRequired":false}),
            now,
        )
        .await
        .map_err(|_| "observed publication audit failed")?;
        insert_event(
            tx,
            id,
            "applied",
            if context.actor_user_id.is_some() {
                "user"
            } else {
                "system"
            },
            context.actor_user_id,
            json!({"targetType":target_type,"targetID":target_id}),
            now,
        )
        .await
        .map_err(|_| "observed publication audit failed")?;
        sqlx::query("SELECT set_config('conduit.governance_publish','',true)")
            .execute(&mut **tx)
            .await
            .map_err(|_| "publication authority reset failed")?;
    }
    Ok(Some(StagedPackage {
        id,
        reused: false,
        item_count: proposals.len(),
        invalid_count,
    }))
}
async fn supersede_observed(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    replacement: Option<i64>,
    context: &OperationContext,
    now: DateTime<Utc>,
) -> Result<(), String> {
    sqlx::query("UPDATE change_sets SET status='superseded',validation_error=NULL,reviewed_at=$2,review_note='superseded by a complete automatic observation',updated_at=$2 WHERE id=$1").bind(id).bind(now).execute(&mut **tx).await.map_err(|e|e.to_string())?;
    insert_event(tx,id,"superseded","system",None,json!({"reason":"complete_observation_changed","replacementChangeSetID":replacement,"replacementOperationID":context.operation_id}),now).await.map_err(|e|e.to_string())
}

#[cfg(test)]
#[path = "wiring_postgres_change_packages_tests.rs"]
mod tests;
