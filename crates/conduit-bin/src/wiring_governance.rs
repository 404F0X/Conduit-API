//! Authoritative policy and permissions shared by every PostgreSQL producer.
use conduit_admin_graphql::{change_set::ChangeSetKind, governance as gql};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};

pub(crate) const KEY: &str = "system_governance_settings";
pub(crate) const LOCK: i64 = 0x434f_4e44_474f_5601;
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Policy {
    pub revision: u64,
    pub enabled: bool,
    pub review_kinds: Vec<String>,
    pub allow_self_review: bool,
    pub legacy_review_scopes: bool,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            revision: 0,
            enabled: true,
            review_kinds: vec![
                "provider_price".into(),
                "model_mapping".into(),
                "retail_price".into(),
            ],
            allow_self_review: true,
            legacy_review_scopes: true,
        }
    }
}
impl Policy {
    pub(crate) fn requires(&self, kind: &str) -> bool {
        self.enabled && self.review_kinds.iter().any(|v| v == kind)
    }
    fn graphql(&self) -> Result<gql::GovernanceSettings, String> {
        Ok(gql::GovernanceSettings {
            revision: self.revision.to_string(),
            enabled: self.enabled,
            review_kinds: self
                .review_kinds
                .iter()
                .map(|s| kind(s))
                .collect::<Result<_, _>>()?,
            allow_self_review: self.allow_self_review,
            legacy_review_scopes: self.legacy_review_scopes,
        })
    }
}
pub(crate) fn kind(value: &str) -> Result<ChangeSetKind, String> {
    Ok(match value {
        "provider_price" => ChangeSetKind::ProviderPrice,
        "model_mapping" => ChangeSetKind::ModelMapping,
        "retail_price" => ChangeSetKind::RetailPrice,
        "channel_configuration" => ChangeSetKind::ChannelConfiguration,
        "model_supply" => ChangeSetKind::ModelSupply,
        "group_access" => ChangeSetKind::GroupAccess,
        "identity_access" => ChangeSetKind::IdentityAccess,
        "project_configuration" => ChangeSetKind::ProjectConfiguration,
        "system_configuration" => ChangeSetKind::SystemConfiguration,
        "project_credit" => ChangeSetKind::ProjectCredit,
        _ => return Err("unknown governance kind".into()),
    })
}
pub(crate) fn domain(kind: &str) -> &'static str {
    match kind {
        "provider_price" | "model_mapping" | "retail_price" => "write_commercialization",
        "channel_configuration" | "model_supply" => "write_channels",
        "group_access" => "write_groups",
        "identity_access" => "write_users",
        "project_configuration" => "write_projects",
        "project_credit" => "grant_credit",
        _ => "write_settings",
    }
}
pub(crate) async fn lock_policy(tx: &mut Transaction<'_, Postgres>) -> Result<Policy, String> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(LOCK)
        .execute(&mut **tx)
        .await
        .map_err(|_| "governance lock failed")?;
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM systems WHERE key=$1 AND deleted_at=0 FOR UPDATE")
            .bind(KEY)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| "governance settings unavailable")?;
    raw.map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|_| "invalid governance settings".into())
        .map(|v| v.unwrap_or_default())
}
pub(crate) async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    actor: i64,
    scope: &str,
) -> Result<(), String> {
    let allowed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users u WHERE u.id=$1 AND u.deleted_at=0 AND u.status='activated' AND (u.is_owner OR u.scopes ? '*' OR u.scopes ? $2 OR EXISTS(SELECT 1 FROM roles r JOIN user_roles ur ON ur.role_id=r.id WHERE ur.user_id=u.id AND r.deleted_at=0 AND (r.project_id IS NULL OR r.project_id=0) AND (r.scopes ? '*' OR r.scopes ? $2))))").bind(actor).bind(scope).fetch_one(&mut **tx).await.map_err(|_|"authorization unavailable")?;
    if allowed {
        Ok(())
    } else {
        Err("FORBIDDEN: current actor lacks the required domain or duty permission".into())
    }
}
pub(crate) async fn authorize_project(
    tx: &mut Transaction<'_, Postgres>,
    actor: i64,
    scope: &str,
    project: i64,
) -> Result<(), String> {
    if authorize(tx, actor, scope).await.is_ok() {
        return Ok(());
    }
    if !conduit_auth::scopes::supports_project_role(scope) {
        return Err("FORBIDDEN: this permission requires a system role".into());
    }
    let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users u JOIN user_projects up ON up.user_id=u.id JOIN projects p ON p.id=up.project_id WHERE u.id=$1 AND p.id=$3 AND u.status='activated' AND u.deleted_at=0 AND p.deleted_at=0 AND p.status='active' AND (up.is_owner OR up.scopes ? $2 OR EXISTS(SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id WHERE ur.user_id=u.id AND r.project_id=p.id AND r.deleted_at=0 AND r.scopes ? $2)))").bind(actor).bind(scope).bind(project).fetch_one(&mut **tx).await.map_err(|_|"project authorization unavailable")?;
    if allowed {
        Ok(())
    } else {
        Err("FORBIDDEN: target Project is outside current permissions".into())
    }
}
pub(crate) async fn review_permission(
    tx: &mut Transaction<'_, Postgres>,
    policy: &Policy,
    actor: i64,
    maker: Option<i64>,
    kind: &str,
    publish: bool,
) -> Result<(), String> {
    if matches!(kind, "provider_price" | "model_mapping" | "retail_price") {
        authorize(tx, actor, domain(kind)).await?;
    }
    if policy.requires(kind) && !policy.legacy_review_scopes {
        authorize(
            tx,
            actor,
            if publish {
                "publish_changes"
            } else {
                "review_changes"
            },
        )
        .await?;
    }
    if policy.requires(kind) && !publish && !policy.allow_self_review && maker == Some(actor) {
        return Err("MAKER_CHECKER_REQUIRED".into());
    }
    Ok(())
}
pub(crate) struct PgGovernanceAdapter {
    pub(crate) pool: PgPool,
}
impl PgGovernanceAdapter {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[cfg(test)]
#[path = "wiring_governance_tests.rs"]
mod tests;

async fn eligible(
    tx: &mut Transaction<'_, Postgres>,
    policy: &Policy,
    except: Option<i64>,
) -> Result<i32, String> {
    let actors:Vec<i64> = sqlx::query_scalar("SELECT id FROM users WHERE status='activated' AND deleted_at=0 AND ($1::bigint IS NULL OR id<>$1) ORDER BY id").bind(except).fetch_all(&mut **tx).await.map_err(|_|"reviewer lookup failed")?;
    let mut count = 0;
    for actor in actors {
        let mut qualifies = true;
        for k in &policy.review_kinds {
            let domains = super::wiring_governance_commands::capabilities()?
                .into_iter()
                .filter(|c| c.kind.as_str() == k)
                .map(|c| c.domain_scope)
                .chain(std::iter::once(domain(k).to_string()));
            for scope in domains {
                if authorize(tx, actor, &scope).await.is_err()
                    || (!policy.legacy_review_scopes
                        && authorize(tx, actor, "review_changes").await.is_err())
                {
                    qualifies = false;
                    break;
                }
            }
            if !qualifies {
                break;
            }
        }
        if qualifies {
            count += 1;
        }
    }
    Ok(count)
}
pub(crate) async fn ensure_policy_viable(
    tx: &mut Transaction<'_, Postgres>,
    p: &Policy,
) -> Result<(), String> {
    if !p.enabled || p.review_kinds.is_empty() {
        return Ok(());
    }
    let actors: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM users WHERE status='activated' AND deleted_at=0 ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| "reviewer lookup failed")?;
    let mut scopes = std::collections::BTreeSet::new();
    for kind in &p.review_kinds {
        if matches!(
            kind.as_str(),
            "provider_price" | "model_mapping" | "retail_price"
        ) {
            scopes.insert(domain(kind).to_string());
        } else {
            for capability in super::wiring_governance_commands::capabilities()?
                .into_iter()
                .filter(|c| c.kind.as_str() == kind)
            {
                scopes.insert(capability.domain_scope);
            }
        }
    }
    let required = if p.allow_self_review { 1 } else { 2 };
    for scope in scopes {
        let mut count = 0;
        for actor in &actors {
            if authorize(tx, *actor, &scope).await.is_ok()
                && (p.legacy_review_scopes || authorize(tx, *actor, "review_changes").await.is_ok())
            {
                count += 1;
            }
        }
        if count < required {
            return Err(
                "NO_ELIGIBLE_REVIEWER: policy would remove a required qualified reviewer".into(),
            );
        }
    }
    Ok(())
}

async fn save_policy(
    tx: &mut Transaction<'_, Postgres>,
    policy: &Policy,
    actor: i64,
    reason: &str,
) -> Result<(), String> {
    sqlx::query("SELECT set_config('conduit.governance_policy_write','authorized',true)")
        .execute(&mut **tx)
        .await
        .map_err(|_| "policy authority unavailable")?;
    let value = serde_json::to_string(policy).map_err(|_| "invalid governance settings")?;
    sqlx::query("INSERT INTO systems(key,value,created_at,updated_at) VALUES($1,$2,now(),now()) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,updated_at=now(),deleted_at=0").bind(KEY).bind(value).execute(&mut **tx).await.map_err(|_|"governance settings write failed")?;
    sqlx::query("INSERT INTO commercial_operation_audits(actor_type,actor_id,operation,result,error_message,created_at) VALUES('user',$1,'governance_policy','success',$2,now())").bind(actor.to_string()).bind(reason).execute(&mut **tx).await.map_err(|_|"governance audit failed")?;
    Ok(())
}

#[async_trait::async_trait]
impl gql::GovernanceServices for PgGovernanceAdapter {
    async fn check_direct(&self, _actor: i64, field: &str) -> Result<(), String> {
        let class = conduit_admin_graphql::governance::mutation_kind(field)?;
        if let Some(kind) = class {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|_| "governance transaction unavailable")?;
            let p = lock_policy(&mut tx).await?;
            if p.requires(kind.as_str()) {
                return Err(format!(
                    "APPROVAL_REQUIRED: {} requires proposeGovernedChange",
                    kind.as_str()
                ));
            }
            tx.commit()
                .await
                .map_err(|_| "governance transaction failed")?;
        }
        Ok(())
    }
    async fn settings(&self) -> Result<gql::GovernanceSettings, String> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "governance transaction unavailable")?;
        let p = lock_policy(&mut tx).await?;
        tx.commit()
            .await
            .map_err(|_| "governance transaction failed")?;
        p.graphql()
    }
    async fn capabilities(&self, actor: i64) -> Result<gql::GovernanceCapabilities, String> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "governance transaction unavailable")?;
        let p = lock_policy(&mut tx).await?;
        let count = eligible(&mut tx, &p, Some(actor)).await?;
        tx.commit()
            .await
            .map_err(|_| "governance transaction failed")?;
        Ok(gql::GovernanceCapabilities {
            actions: super::wiring_governance_commands::capabilities()?,
            review_scope: "review_changes".into(),
            publish_scope: "publish_changes".into(),
            policy_scope: "manage_governance".into(),
            eligible_reviewer_count: count,
        })
    }
    async fn target_revision(
        &self,
        actor: i64,
        action: String,
        targets: Vec<async_graphql::ID>,
        payload: async_graphql::Json<serde_json::Value>,
    ) -> Result<String, String> {
        super::wiring_governance_commands::base(&self.pool, actor, &action, targets, payload.0)
            .await
    }
    async fn readable_kinds(&self, actor: i64) -> Result<Vec<ChangeSetKind>, String> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "governance transaction unavailable")?;
        let mut result = Vec::new();
        for k in [
            "provider_price",
            "model_mapping",
            "retail_price",
            "channel_configuration",
            "model_supply",
            "group_access",
            "identity_access",
            "project_configuration",
            "system_configuration",
            "project_credit",
        ] {
            let scopes = super::wiring_governance_commands::capabilities()?
                .into_iter()
                .filter(|v| v.kind.as_str() == k)
                .map(|v| v.domain_scope)
                .chain(std::iter::once(domain(k).to_string()));
            for scope in scopes {
                let read = if scope == "grant_credit" {
                    "read_billing".into()
                } else {
                    scope.replace("write_", "read_")
                };
                let project_read:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_projects up WHERE up.user_id=$1 AND conduit_can_read_domain($1,$2,up.project_id))").bind(actor).bind(&scope).fetch_one(&mut *tx).await.map_err(|_|"permission read failed")?;
                if project_read
                    || authorize(&mut tx, actor, &scope).await.is_ok()
                    || authorize(&mut tx, actor, &read).await.is_ok()
                {
                    result.push(kind(k)?);
                    break;
                }
            }
        }
        tx.commit().await.map_err(|_| "permission read failed")?;
        Ok(result)
    }
    async fn authorize_read(&self, actor: i64, id: async_graphql::ID) -> Result<(), String> {
        let id = id
            .as_str()
            .parse::<i64>()
            .map_err(|_| "invalid package ID")?;
        let allowed: bool = sqlx::query_scalar("SELECT conduit_can_read_change_set($1,$2)")
            .bind(actor)
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| "package read failed")?;
        if allowed {
            Ok(())
        } else {
            Err("FORBIDDEN: package domain or Project is outside current permissions".into())
        }
    }
    async fn update_settings(
        &self,
        actor: i64,
        input: gql::UpdateGovernanceSettingsInput,
        expected: String,
    ) -> Result<gql::GovernanceSettings, String> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "governance transaction unavailable")?;
        let old = lock_policy(&mut tx).await?;
        authorize(&mut tx, actor, "manage_governance").await?;
        if old.revision.to_string() != expected {
            return Err("REVISION_CONFLICT".into());
        }
        if input.enabled && !old.enabled && input.allow_self_review.is_none() {
            return Err("SELF_REVIEW_CHOICE_REQUIRED".into());
        }
        let p = Policy {
            revision: old.revision + 1,
            enabled: input.enabled,
            review_kinds: input
                .review_kinds
                .into_iter()
                .map(|k| k.as_str().into())
                .collect(),
            allow_self_review: input.allow_self_review.unwrap_or(old.allow_self_review),
            legacy_review_scopes: false,
        };
        if old.requires("system_configuration")
            && (!p.enabled
                || p.allow_self_review && !old.allow_self_review
                || old.review_kinds.iter().any(|k| !p.review_kinds.contains(k)))
        {
            return Err(
                "APPROVAL_REQUIRED: governance policy reduction requires a governed proposal"
                    .into(),
            );
        }
        ensure_policy_viable(&mut tx, &p).await?;
        save_policy(&mut tx, &p, actor, "explicit governance settings update").await?;
        tx.commit().await.map_err(|_| "governance commit failed")?;
        p.graphql()
    }
    async fn propose(
        &self,
        actor: i64,
        input: gql::ProposeGovernedChangeInput,
    ) -> Result<conduit_admin_graphql::change_package::ChangeSetDetail, String> {
        super::wiring_governance_commands::propose(&self.pool, actor, input).await
    }
    async fn validate(
        &self,
        actor: i64,
        id: async_graphql::ID,
        expected: String,
    ) -> Result<conduit_admin_graphql::change_package::ChangeSetDetail, String> {
        super::wiring_governance_commands::validate(&self.pool, actor, id, expected).await
    }
    async fn recover(
        &self,
        actor: i64,
        reason: String,
        expected: String,
    ) -> Result<gql::GovernanceSettings, String> {
        if reason.trim().is_empty() {
            return Err("reason is required".into());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "governance transaction unavailable")?;
        let mut p = lock_policy(&mut tx).await?;
        let owner:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND is_owner AND status='activated' AND deleted_at=0)").bind(actor).fetch_one(&mut *tx).await.map_err(|_|"authorization unavailable")?;
        if !owner {
            return Err("FORBIDDEN".into());
        }
        if p.revision.to_string() != expected {
            return Err("REVISION_CONFLICT".into());
        }
        if ensure_policy_viable(&mut tx, &p).await.is_ok() {
            return Err("policy recovery requires a missing qualified reviewer".into());
        }
        p.enabled = false;
        p.revision += 1;
        save_policy(&mut tx, &p, actor, &reason).await?;
        tx.commit().await.map_err(|_| "governance commit failed")?;
        p.graphql()
    }
    async fn claim_secrets(
        &self,
        actor: i64,
        id: async_graphql::ID,
    ) -> Result<async_graphql::Json<serde_json::Value>, String> {
        super::wiring_governance_commands::claim_secrets(&self.pool, actor, id)
            .await
            .map(async_graphql::Json)
    }
    async fn emergency(
        &self,
        actor: i64,
        target: String,
        id: async_graphql::ID,
        reason: String,
    ) -> Result<bool, String> {
        if reason.trim().is_empty() {
            return Err("reason is required".into());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "governance transaction unavailable")?;
        lock_policy(&mut tx).await?;
        authorize(&mut tx, actor, "emergency_revoke").await?;
        let (table, status, scope) = match target.as_str() {
            "channel" => ("channels", "disabled", "write_channels"),
            "user" => ("users", "deactivated", "write_users"),
            "api_key" => ("api_keys", "disabled", "write_api_keys"),
            _ => return Err("unsupported emergency target".into()),
        };
        authorize(&mut tx, actor, scope).await?;
        let id = id.as_str().parse::<i64>().map_err(|_| "invalid target")?;
        sqlx::query("SELECT set_config('conduit.emergency_restriction','authorized',true)")
            .execute(&mut *tx)
            .await
            .map_err(|_| "emergency authority unavailable")?;
        let result = sqlx::query(sqlx::AssertSqlSafe::<&str>(&format!(
            "UPDATE {table} SET status=$1,updated_at=now() WHERE id=$2 AND deleted_at=0"
        )))
        .bind(status)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|_| "emergency restriction failed")?;
        sqlx::query("INSERT INTO commercial_operation_audits(actor_type,actor_id,operation,result,error_message,created_at) VALUES('user',$1,'emergency_restrict_access','success',$2,now())").bind(actor.to_string()).bind(reason).execute(&mut *tx).await.map_err(|_|"emergency audit failed")?;
        tx.commit().await.map_err(|_| "emergency commit failed")?;
        Ok(result.rows_affected() > 0)
    }
}
