//! Independent model scopes and user recharge classification. Project wallets stay separate.
use async_graphql::ID;
use conduit_admin_graphql::groups::*;
use sqlx::{PgPool, Row};
use std::collections::BTreeSet;
#[derive(Clone)]
pub struct PgGroupAdapter {
    pool: PgPool,
}
fn storage(_: sqlx::Error) -> String {
    "group storage operation failed".into()
}
fn status(s: &str) -> GroupStatus {
    match s {
        "enabled" => GroupStatus::Enabled,
        "disabled" => GroupStatus::Disabled,
        _ => GroupStatus::Archived,
    }
}
impl PgGroupAdapter {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn model(&self, id: &str) -> Result<ModelGroup, String> {
        let r =
            sqlx::query("SELECT id,name,description,status,revision FROM model_groups WHERE id=$1")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        let models:Vec<String>=sqlx::query_scalar("SELECT public_model_key FROM model_group_models WHERE model_group_id=$1 ORDER BY public_model_key").bind(id).fetch_all(&self.pool).await.map_err(storage)?;
        Ok(ModelGroup {
            is_default: id == DEFAULT_MODEL_GROUP_ID,
            id: ID(r.get("id")),
            name: r.get("name"),
            description: r.get("description"),
            status: status(r.get("status")),
            revision: r.get("revision"),
            model_ids: models.into_iter().map(ID).collect(),
        })
    }
    async fn user(&self, id: &str) -> Result<UserGroup, String> {
        let r = sqlx::query("SELECT id,name,description,status,recharge_multiplier_ppm,recharge_revision FROM user_groups WHERE id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
        let users: Vec<i64> = sqlx::query_scalar(
            "SELECT user_id FROM user_group_users WHERE user_group_id=$1 ORDER BY user_id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let models:Vec<String>=sqlx::query_scalar("SELECT model_group_id FROM user_group_model_groups WHERE user_group_id=$1 ORDER BY model_group_id").bind(id).fetch_all(&self.pool).await.map_err(storage)?;
        Ok(UserGroup {
            id: ID(r.get("id")),
            name: r.get("name"),
            description: r.get("description"),
            status: status(r.get("status")),
            recharge_multiplier_ppm: r.get::<i64, _>("recharge_multiplier_ppm").to_string(),
            recharge_revision: r.get::<i64, _>("recharge_revision").to_string(),
            member_user_ids: users
                .into_iter()
                .map(|u| ID(format!("gid://conduit/User/{u}")))
                .collect(),
            model_group_ids: models.into_iter().map(ID).collect(),
        })
    }
}
impl PgGroupAdapter {
    async fn member_catalog(
        &self,
        key: Option<i64>,
        user: Option<i64>,
        group: Option<String>,
        project: i64,
    ) -> Result<APIKeyProfileCatalog, String> {
        let mut out = APIKeyProfileCatalog {
            key_id: key.map(|id| ID(id.to_string())),
            project_id: ID(project.to_string()),
            billing_user_id: user.map(|id| ID(id.to_string())),
            model_group_id: group.clone().map(ID),
            configured: false,
            model_ids: vec![],
            channels: vec![],
            models: vec![],
        };
        let (Some(user), Some(group)) = (user, group) else {
            return Ok(out);
        };
        // Use the target billing member, including disabled Keys that administrators may configure.
        // Current profile restrictions intentionally do not hide legitimate expansion candidates.
        let Some(allowed) = self
            .assignable(user, project)
            .await?
            .into_iter()
            .find(|g| g.id.as_str() == group)
        else {
            return Ok(out);
        };
        let access = crate::wiring_project_access::resolve_effective_project_access_postgres(
            &self.pool, project,
        )
        .await
        .map_err(|_| "Project access unavailable")?;
        let mut channel_ids = BTreeSet::new();
        for model in allowed.model_ids {
            if let Some(channels) = access.routes_by_model.get(model.as_str()) {
                channel_ids.extend(channels.iter().copied());
                out.model_ids.push(model.to_string());
            }
        }
        let rows:Vec<(i64,String)>=sqlx::query_as("SELECT id,name FROM channels WHERE id=ANY($1) AND deleted_at=0 AND status='enabled' ORDER BY name,id").bind(channel_ids.into_iter().collect::<Vec<_>>()).fetch_all(&self.pool).await.map_err(storage)?;
        out.channels = rows
            .into_iter()
            .map(|(id, name)| APIKeyProfileChannel {
                id: ID(id.to_string()),
                name,
            })
            .collect();
        let names:Vec<(String,String)>=sqlx::query_as("SELECT model_id,name FROM models WHERE model_id=ANY($1) AND deleted_at=0 AND status='enabled'").bind(&out.model_ids).fetch_all(&self.pool).await.map_err(storage)?;
        for id in &out.model_ids {
            out.models.push(APIKeyProfileModel {
                id: id.clone(),
                name: names
                    .iter()
                    .find(|(key, _)| key == id)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_else(|| id.clone()),
                channel_ids: access
                    .routes_by_model
                    .get(id)
                    .into_iter()
                    .flatten()
                    .map(|id| ID(id.to_string()))
                    .collect(),
            });
        }
        out.configured = !out.model_ids.is_empty() && !out.channels.is_empty();
        Ok(out)
    }
}
#[async_trait::async_trait]
impl GroupServices for PgGroupAdapter {
    async fn profile_catalog(
        &self,
        key: i64,
        project: i64,
    ) -> Result<APIKeyProfileCatalog, String> {
        let row:Option<(Option<i64>,Option<String>)>=sqlx::query_as("SELECT k.user_id,g.model_group_id FROM api_keys k LEFT JOIN api_key_model_groups g ON g.api_key_id=k.id WHERE k.id=$1 AND k.project_id=$2 AND k.deleted_at=0 AND k.\"type\"<>'noauth'").bind(key).bind(project).fetch_optional(&self.pool).await.map_err(storage)?;
        let (user, group) = row.ok_or("Key not found in selected Project")?;
        self.member_catalog(Some(key), user, group, project).await
    }
    async fn creation_catalog(
        &self,
        user: Option<i64>,
        parent: Option<i64>,
        project: i64,
        group: &str,
    ) -> Result<APIKeyProfileCatalog, String> {
        let user = if let Some(user) = user {
            user
        } else {
            let parent = parent.ok_or("billing member required")?;
            let owner:Option<Option<i64>>=sqlx::query_scalar("SELECT user_id FROM api_keys WHERE id=$1 AND project_id=$2 AND status='enabled' AND deleted_at=0").bind(parent).bind(project).fetch_optional(&self.pool).await.map_err(storage)?;
            owner
                .flatten()
                .ok_or("calling Key billing member unavailable")?
        };
        self.member_catalog(None, Some(user), Some(group.to_owned()), project)
            .await
    }

    async fn default_model_group_diagnostic(
        &self,
    ) -> Result<Option<DefaultModelGroupDiagnostic>, String> {
        let active = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM systems WHERE key=$1 AND deleted_at=0)",
        )
        .bind(DEFAULT_CONFLICT_KEY)
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        Ok(active.then(|| DefaultModelGroupDiagnostic {
            code: "DEFAULT_MODEL_GROUP_CONFLICT".into(),
            message: "Another model group is named Default. Rename it, then restart to initialize the reserved group.".into(),
        }))
    }

    async fn model_groups(&self) -> Result<Vec<ModelGroup>, String> {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM model_groups ORDER BY name,id")
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        let mut out = vec![];
        for id in ids {
            out.push(self.model(&id).await?)
        }
        Ok(out)
    }
    async fn user_groups(&self) -> Result<Vec<UserGroup>, String> {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM user_groups ORDER BY name,id")
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        let mut out = vec![];
        for id in ids {
            out.push(self.user(&id).await?)
        }
        Ok(out)
    }
    async fn save_model_group(&self, input: SaveModelGroupInput) -> Result<ModelGroup, String> {
        let id = input
            .id
            .map(|id| id.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if input.name.trim().is_empty() {
            return Err("group name required".into());
        }
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SELECT pg_advisory_xact_lock(4850181378862437889)")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let models = input
            .model_ids
            .into_iter()
            .map(|id| id.to_string())
            .collect::<BTreeSet<_>>();
        for model in &models {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM models WHERE model_id=$1 AND deleted_at=0)",
            )
            .bind(model)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
            if !exists {
                return Err("unknown public model".into());
            }
        }
        sqlx::query("INSERT INTO model_groups(id,name,description,status) VALUES($1,$2,$3,$4) ON CONFLICT(id) DO UPDATE SET name=$2,description=$3,status=$4,revision=model_groups.revision+1,updated_at=now()").bind(&id).bind(input.name.trim()).bind(input.description).bind(input.status.as_str()).execute(&mut *tx).await.map_err(storage)?;
        sqlx::query("DELETE FROM model_group_models WHERE model_group_id=$1")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        for model in models {
            sqlx::query(
                "INSERT INTO model_group_models(model_group_id,public_model_key) VALUES($1,$2)",
            )
            .bind(&id)
            .bind(model)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }
        tx.commit().await.map_err(storage)?;
        self.model(&id).await
    }
    async fn save_user_group(&self, input: SaveUserGroupInput) -> Result<UserGroup, String> {
        let id = input
            .id
            .map(|id| id.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if input.name.trim().is_empty() {
            return Err("group name required".into());
        }
        let users = input
            .member_user_ids
            .into_iter()
            .map(|u| numeric_id(u.as_str()))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SELECT pg_advisory_xact_lock(4850181378862437889)")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let existing: Option<(i64,i64)> = sqlx::query_as("SELECT recharge_multiplier_ppm,recharge_revision FROM user_groups WHERE id=$1 FOR UPDATE").bind(&id).fetch_optional(&mut *tx).await.map_err(storage)?;
        let parse_positive = |raw: &str| -> Result<i64, String> {
            if raw.is_empty() || raw.starts_with('0') || !raw.bytes().all(|b| b.is_ascii_digit()) {
                return Err("invalid recharge integer".into());
            }
            raw.parse::<i64>()
                .map_err(|_| "recharge integer overflow".into())
        };
        let desired_ppm = input
            .recharge_multiplier_ppm
            .as_deref()
            .map(parse_positive)
            .transpose()?;
        let expected = input
            .expected_recharge_revision
            .as_deref()
            .map(parse_positive)
            .transpose()?;
        if desired_ppm.is_some_and(|p| !(1..=1_000_000_000).contains(&p)) {
            return Err("recharge multiplier must be 1..1000000000 PPM".into());
        }
        if let Some((_, revision)) = existing {
            if (desired_ppm.is_some() && expected.is_none())
                || expected.is_some_and(|v| v != revision)
            {
                return Err("recharge revision conflict; reload UserGroup".into());
            }
        } else if expected.is_some() {
            return Err("recharge revision conflict; group does not exist".into());
        }
        for user in &users {
            let conflict:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_group_users WHERE user_id=$1 AND user_group_id<>$2)").bind(user).bind(&id).fetch_one(&mut *tx).await.map_err(storage)?;
            if conflict {
                return Err("user already belongs to another UserGroup; move explicitly".into());
            }
        }
        sqlx::query("INSERT INTO user_groups(id,name,description,status) VALUES($1,$2,$3,$4) ON CONFLICT(id) DO UPDATE SET name=$2,description=$3,status=$4,updated_at=now()").bind(&id).bind(input.name.trim()).bind(input.description).bind(input.status.as_str()).execute(&mut *tx).await.map_err(storage)?;
        if let Some(ppm) = desired_ppm {
            sqlx::query("UPDATE user_groups SET recharge_multiplier_ppm=$2,recharge_revision=recharge_revision+CASE WHEN recharge_multiplier_ppm<>$2 AND $3 THEN 1 ELSE 0 END WHERE id=$1")
                .bind(&id).bind(ppm).bind(existing.is_some()).execute(&mut *tx).await.map_err(storage)?;
        }
        sqlx::query("DELETE FROM user_group_users WHERE user_group_id=$1")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        for user in users {
            sqlx::query("INSERT INTO user_group_users(user_id,user_group_id) VALUES($1,$2)")
                .bind(user)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
        }
        sqlx::query("DELETE FROM user_group_model_groups WHERE user_group_id=$1")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        for model in input
            .model_group_ids
            .into_iter()
            .map(|id| id.to_string())
            .collect::<BTreeSet<_>>()
        {
            sqlx::query(
                "INSERT INTO user_group_model_groups(user_group_id,model_group_id) VALUES($1,$2)",
            )
            .bind(&id)
            .bind(model)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }
        tx.commit().await.map_err(storage)?;
        self.user(&id).await
    }
    async fn move_user(&self, user: i64, group: &str) -> Result<bool, String> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SELECT pg_advisory_xact_lock(4850181378862437889)")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("INSERT INTO user_group_users(user_id,user_group_id) VALUES($1,$2) ON CONFLICT(user_id) DO UPDATE SET user_group_id=$2").bind(user).bind(group).execute(&mut *tx).await.map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(true)
    }
    async fn assignable(&self, user: i64, project: i64) -> Result<Vec<ModelGroup>, String> {
        let ids:Vec<String>=sqlx::query_scalar("SELECT m.id FROM model_groups m JOIN user_group_model_groups g ON g.model_group_id=m.id JOIN user_groups u ON u.id=g.user_group_id JOIN user_group_users member ON member.user_group_id=u.id WHERE member.user_id=$1 AND m.status='enabled' AND u.status='enabled' AND EXISTS(SELECT 1 FROM user_projects WHERE user_id=$1 AND project_id=$2) ORDER BY m.name,m.id").bind(user).bind(project).fetch_all(&self.pool).await.map_err(storage)?;
        let access = crate::wiring_project_access::resolve_effective_project_access_postgres(
            &self.pool, project,
        )
        .await
        .map_err(|_| "Project access unavailable")?;
        let mut out = vec![];
        for id in ids {
            let mut model = self.model(&id).await?;
            model.model_ids.retain(|id| {
                access
                    .routes_by_model
                    .get(id.as_str())
                    .is_some_and(|routes| !routes.is_empty())
            });
            if !model.model_ids.is_empty() {
                out.push(model)
            }
        }
        Ok(out)
    }
    async fn assignable_for_key(&self, key: i64, project: i64) -> Result<Vec<ModelGroup>, String> {
        let user: Option<Option<i64>> = sqlx::query_scalar(
            "SELECT user_id FROM api_keys WHERE id=$1 AND project_id=$2 AND deleted_at=0",
        )
        .bind(key)
        .bind(project)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        self.assignable(
            user.flatten().ok_or("key billing member required")?,
            project,
        )
        .await
    }
    async fn key_group(&self, key: i64, project: i64) -> Result<Option<ID>, String> {
        let id:Option<String>=sqlx::query_scalar("SELECT g.model_group_id FROM api_key_model_groups g JOIN api_keys k ON k.id=g.api_key_id WHERE k.id=$1 AND k.project_id=$2 AND k.deleted_at=0").bind(key).bind(project).fetch_optional(&self.pool).await.map_err(storage)?;
        Ok(id.map(ID))
    }
    async fn assign_key(&self, key: i64, project: i64, group: &str) -> Result<bool, String> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SELECT pg_advisory_xact_lock(4850181378862437889)")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let user:Option<Option<i64>>=sqlx::query_scalar("SELECT user_id FROM api_keys WHERE id=$1 AND project_id=$2 AND deleted_at=0 AND \"type\"<>'noauth' FOR UPDATE").bind(key).bind(project).fetch_optional(&mut *tx).await.map_err(storage)?;
        let user = user.flatten().ok_or("key billing member required")?;
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM model_groups m JOIN user_group_model_groups g ON g.model_group_id=m.id JOIN user_groups u ON u.id=g.user_group_id JOIN user_group_users member ON member.user_group_id=u.id JOIN user_projects p ON p.user_id=member.user_id WHERE m.id=$1 AND member.user_id=$2 AND p.project_id=$3 AND m.status='enabled' AND u.status='enabled' AND EXISTS(SELECT 1 FROM model_group_models mm WHERE mm.model_group_id=m.id))").bind(group).bind(user).bind(project).fetch_one(&mut *tx).await.map_err(storage)?;
        if !valid {
            return Err("ModelGroup is not allowed for the key billing member".into());
        }
        sqlx::query("INSERT INTO api_key_model_groups(api_key_id,model_group_id) VALUES($1,$2) ON CONFLICT(api_key_id) DO UPDATE SET model_group_id=$2").bind(key).bind(group).execute(&mut *tx).await.map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(true)
    }
}

/// One statement binds the stored Key, its real billing member and current grants.
/// Callers cannot select a different group through a body field.
pub(crate) async fn trusted_key_group(
    pool: &PgPool,
    key: i64,
    user: i64,
    project: i64,
) -> Result<Option<(String, i64, Vec<String>)>, sqlx::Error> {
    sqlx::query_as("SELECT m.id,m.revision,array_agg(mm.public_model_key ORDER BY mm.public_model_key) FROM api_keys original_key JOIN api_key_model_groups k ON k.api_key_id=original_key.id JOIN model_groups m ON m.id=k.model_group_id JOIN model_group_models mm ON mm.model_group_id=m.id JOIN user_group_model_groups grant_group ON grant_group.model_group_id=m.id JOIN user_groups ug ON ug.id=grant_group.user_group_id JOIN user_group_users member ON member.user_group_id=ug.id JOIN user_projects p ON p.user_id=member.user_id AND p.project_id=original_key.project_id WHERE k.api_key_id=$1 AND member.user_id=$2 AND original_key.user_id=$2 AND original_key.project_id=$3 AND original_key.deleted_at=0 AND original_key.status='enabled' AND m.status='enabled' AND ug.status='enabled' GROUP BY m.id,m.revision").bind(key).bind(user).bind(project).fetch_optional(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_db::repo::api_key_repo::CreateApiKeyInput;
    use conduit_db::repo::{ApiKeyRepo, RequestContext};
    use conduit_db::{PgApiKeyRepo, PolicyContext, Principal};
    #[tokio::test]
    async fn default_model_group_collision_recovers_without_adopting_existing_group()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let groups = PgGroupAdapter::new(db.pool.clone());
        sqlx::query("INSERT INTO model_groups(id,name) VALUES('existing','Default')")
            .execute(&db.pool)
            .await?;
        sqlx::query("INSERT INTO model_group_models VALUES('existing','original-model')")
            .execute(&db.pool)
            .await?;
        initialize_default_model_group(&db.pool).await?;
        initialize_default_model_group(&db.pool).await?;
        assert_eq!(
            groups.default_model_group_diagnostic().await?.unwrap().code,
            "DEFAULT_MODEL_GROUP_CONFLICT"
        );
        let existing = groups.model_groups().await?;
        assert_eq!(existing.len(), 1);
        assert!(!existing[0].is_default);
        assert_eq!(existing[0].model_ids, vec![ID("original-model".into())]);

        sqlx::query("UPDATE model_groups SET name='Previously Default' WHERE id='existing'")
            .execute(&db.pool)
            .await?;
        initialize_default_model_group(&db.pool).await?;
        assert!(groups.default_model_group_diagnostic().await?.is_none());
        let group = groups.model(DEFAULT_MODEL_GROUP_ID).await?;
        assert_eq!(group.name, "Default");
        assert!(group.is_default);
        assert_eq!(group.status, GroupStatus::Enabled);
        assert!(group.model_ids.is_empty());
        sqlx::query("UPDATE model_groups SET name='Custom Default' WHERE id=$1")
            .bind(DEFAULT_MODEL_GROUP_ID)
            .execute(&db.pool)
            .await?;
        sqlx::query("INSERT INTO model_group_models VALUES($1,'chosen-model')")
            .bind(DEFAULT_MODEL_GROUP_ID)
            .execute(&db.pool)
            .await?;
        initialize_default_model_group(&db.pool).await?;
        let group = groups.model(DEFAULT_MODEL_GROUP_ID).await?;
        assert_eq!(group.name, "Custom Default");
        assert_eq!(group.model_ids, vec![ID("chosen-model".into())]);
        assert_eq!(
            groups.model("existing").await?.model_ids,
            vec![ID("original-model".into())]
        );
        db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn independent_groups_permissions_revocation_and_atomic_keys_when_dsn_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let isolated = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = isolated.pool.clone();
        let result:Result<(),Box<dyn std::error::Error>>=async{
 let user:i64=sqlx::query_scalar("INSERT INTO users(email,status,password) VALUES('groups-user@test','activated','unused') RETURNING id").fetch_one(&pool).await?;
 let other:i64=sqlx::query_scalar("INSERT INTO users(email,status,password) VALUES('groups-other@test','activated','unused') RETURNING id").fetch_one(&pool).await?;
 let project:i64=sqlx::query_scalar("INSERT INTO projects(name,status,profiles) VALUES('Groups Test','active','{}') RETURNING id").fetch_one(&pool).await?;
 sqlx::query("INSERT INTO user_projects(user_id,project_id,is_owner) VALUES($1,$2,TRUE),($3,$2,FALSE)").bind(user).bind(project).bind(other).execute(&pool).await?;
 sqlx::query("INSERT INTO models(developer,model_id,type,name,icon,\"group\",model_card,settings,status) VALUES('test','allowed','chat','Allowed','','test','{}','{}','enabled'),('test','second','chat','Second','','test','{}','{}','enabled')").execute(&pool).await?;

 let channel:i64=sqlx::query_scalar("INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) VALUES('openai','group-route','enabled','{}','[]','') RETURNING id").fetch_one(&pool).await?;
 let deployment:i64=sqlx::query_scalar("INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,status) VALUES($1,'provider-model','provider-model','enabled') RETURNING id").bind(channel).fetch_one(&pool).await?;
 let models:Vec<i64>=sqlx::query_scalar("SELECT id FROM models WHERE model_id IN ('allowed','second')").fetch_all(&pool).await?;
 for model in models {
 sqlx::query("INSERT INTO model_routes(public_model_id,deployment_id,status) VALUES($1,$2,'enabled')").bind(model).bind(deployment).execute(&pool).await?;
 sqlx::query("INSERT INTO project_entitlement_overrides(project_id,public_model_id,effect,source_type,status,created_at,updated_at) VALUES($1,$2,'allow','test','active',now(),now())").bind(project).bind(model).execute(&pool).await?;
 }
 let adapter=PgGroupAdapter::new(pool.clone());
 let group=adapter.save_model_group(SaveModelGroupInput{id:None,name:"Model scope".into(),description:None,status:GroupStatus::Enabled,model_ids:vec![ID("allowed".into())]}).await?;
 let ug=adapter.save_user_group(SaveUserGroupInput{recharge_multiplier_ppm:None,expected_recharge_revision:None,id:None,name:"Users".into(),description:None,status:GroupStatus::Enabled,member_user_ids:vec![ID(format!("gid://conduit/User/{user}"))],model_group_ids:vec![group.id.clone()]}).await?;
 let second=adapter.save_user_group(SaveUserGroupInput{recharge_multiplier_ppm:None,expected_recharge_revision:None,id:None,name:"Other users".into(),description:None,status:GroupStatus::Enabled,member_user_ids:vec![],model_group_ids:vec![]}).await?;
 assert!(adapter.save_user_group(SaveUserGroupInput{recharge_multiplier_ppm:None,expected_recharge_revision:None,id:Some(second.id.clone()),name:"Other users".into(),description:None,status:GroupStatus::Enabled,member_user_ids:vec![ID(user.to_string())],model_group_ids:vec![]}).await.is_err());
 assert_eq!(adapter.user(&ug.id).await?.member_user_ids.len(),1);
 let repo=PgApiKeyRepo::new(pool.clone());let ctx=RequestContext::new(PolicyContext::new(Principal::test()));
 let input=|name:&str,owner:i64,g:Option<String>|CreateApiKeyInput{model_group_id:g,id:String::new(),user_id:Some(owner.to_string()),project_id:project.to_string(),name:name.into(),key:format!("key-{name}"),key_type:"user".into(),scopes:vec![],profiles:None,created_at:chrono::Utc::now().to_rfc3339()};

 use crate::wiring_postgres_auth::{PgAuthUserRepo,PgAuthApiKeyRepo,PgApiKeyValidationService};
 use conduit_http::middleware::api_key_auth::{ApiKeyValidationService,ApiKeyValidationError};
 let validator=PgApiKeyValidationService::new(std::sync::Arc::new(conduit_services::AuthService::new(std::sync::Arc::new(PgAuthUserRepo::new(pool.clone())),std::sync::Arc::new(PgAuthApiKeyRepo::new(pool.clone())),b"group-test-jwt".to_vec())),std::sync::Arc::new(PgApiKeyRepo::new(pool.clone())),pool.clone());
 let old=repo.create_api_key(&ctx,input("legacy",user,None)).await?;
 let old_id=old.id.parse()?;
 assert!(trusted_key_group(&pool,old_id,user,project).await?.is_none());
 assert_eq!(validator.validate("key-legacy").await,Err(ApiKeyValidationError::Invalid));
 // Actor authority never substitutes for the key's actual billing member.
 assert!(repo.create_api_key(&ctx,input("denied",other,Some(group.id.to_string()))).await.is_err());
 let denied_count:i64=sqlx::query_scalar("SELECT count(*) FROM api_keys WHERE name='denied'").fetch_one(&pool).await?;assert_eq!(denied_count,0);
 let key=repo.create_api_key(&ctx,input("configured",user,Some(group.id.to_string()))).await?;let key_id=key.id.parse()?;
 let metadata=validator.validate("key-configured").await.map_err(|e|format!("valid group auth: {e:?}"))?; assert_eq!(metadata.model_group_id,group.id.as_str());assert_eq!(metadata.allowed_models,"allowed");assert_eq!(metadata.project_channels_by_model.get("allowed"),Some(&vec![channel]));assert_eq!(metadata.project_upstream_models_by_model["allowed"][&channel],"provider-model");
 let trusted=trusted_key_group(&pool,key_id,user,project).await?.unwrap();assert_eq!(trusted.0,group.id.as_str());assert_eq!(trusted.1,1);assert_eq!(trusted.2,vec!["allowed"]);
 assert!(trusted_key_group(&pool,key_id,other,project).await?.is_none());assert!(trusted_key_group(&pool,key_id,user,project+999).await?.is_none());
 adapter.assign_key(old_id,project,group.id.as_str()).await?;assert!(trusted_key_group(&pool,old_id,user,project).await?.is_some());

 sqlx::query("UPDATE api_keys SET profiles=$1 WHERE id=$2").bind(serde_json::json!({"activeProfile":"limited","profiles":[{"name":"limited","modelIDs":["second"]}]})).bind(key_id).execute(&pool).await?;
 assert_eq!(validator.validate("key-configured").await,Err(ApiKeyValidationError::Invalid));
 sqlx::query("UPDATE api_keys SET profiles='{}' WHERE id=$1").bind(key_id).execute(&pool).await?;
 let updated=adapter.save_model_group(SaveModelGroupInput{id:Some(group.id.clone()),name:group.name.clone(),description:None,status:GroupStatus::Enabled,model_ids:vec![ID("second".into())]}).await?;
 let trusted=trusted_key_group(&pool,key_id,user,project).await?.unwrap();assert_eq!(trusted.1,updated.revision);assert!(updated.revision>group.revision);assert_eq!(trusted.2,vec!["second"]);
 adapter.save_model_group(SaveModelGroupInput{id:Some(group.id.clone()),name:group.name.clone(),description:None,status:GroupStatus::Enabled,model_ids:vec![]}).await?;assert!(trusted_key_group(&pool,key_id,user,project).await?.is_none()); assert_eq!(validator.validate("key-configured").await,Err(ApiKeyValidationError::Invalid));
 adapter.save_model_group(SaveModelGroupInput{id:Some(group.id.clone()),name:group.name.clone(),description:None,status:GroupStatus::Disabled,model_ids:vec![ID("allowed".into())]}).await?;assert!(trusted_key_group(&pool,key_id,user,project).await?.is_none()); assert_eq!(validator.validate("key-configured").await,Err(ApiKeyValidationError::Invalid));

            adapter.save_model_group(SaveModelGroupInput{id:Some(group.id.clone()),name:group.name.clone(),description:None,status:GroupStatus::Enabled,model_ids:vec![ID("allowed".into())]}).await?;
            assert!(trusted_key_group(&pool,key_id,user,project).await?.is_some());
            assert!(validator.validate("key-configured").await.is_ok());
            adapter.save_user_group(SaveUserGroupInput{recharge_multiplier_ppm:None,expected_recharge_revision:None,id:Some(ug.id.clone()),name:ug.name.clone(),description:None,status:GroupStatus::Enabled,member_user_ids:vec![ID(user.to_string())],model_group_ids:vec![]}).await?;
            assert!(trusted_key_group(&pool,key_id,user,project).await?.is_none()); assert_eq!(validator.validate("key-configured").await,Err(ApiKeyValidationError::Invalid));
            adapter.save_user_group(SaveUserGroupInput{recharge_multiplier_ppm:None,expected_recharge_revision:None,id:Some(ug.id.clone()),name:ug.name.clone(),description:None,status:GroupStatus::Disabled,member_user_ids:vec![ID(user.to_string())],model_group_ids:vec![group.id.clone()]}).await?;
            assert!(trusted_key_group(&pool,key_id,user,project).await?.is_none()); assert_eq!(validator.validate("key-configured").await,Err(ApiKeyValidationError::Invalid));
            adapter.save_user_group(SaveUserGroupInput{recharge_multiplier_ppm:None,expected_recharge_revision:None,id:Some(ug.id.clone()),name:ug.name.clone(),description:None,status:GroupStatus::Enabled,member_user_ids:vec![ID(user.to_string())],model_group_ids:vec![group.id.clone()]}).await?;
            assert!(trusted_key_group(&pool,key_id,user,project).await?.is_some());
            assert!(validator.validate("key-configured").await.is_ok());
 adapter.move_user(user,second.id.as_str()).await?;assert!(trusted_key_group(&pool,key_id,user,project).await?.is_none()); assert_eq!(validator.validate("key-configured").await,Err(ApiKeyValidationError::Invalid));assert_eq!(adapter.user(&ug.id).await?.member_user_ids.len(),0);
 let assignments:i64=sqlx::query_scalar("SELECT count(*) FROM api_key_model_groups WHERE api_key_id=$1").bind(key_id).fetch_one(&pool).await?;assert_eq!(assignments,1);
 Ok(())}.await;
        isolated.cleanup().await?;
        result
    }
    #[tokio::test]
    async fn key_profile_catalog_management_scope_when_dsn_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let isolated = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = isolated.pool.clone();
        let result:Result<(),Box<dyn std::error::Error>>=async{
            macro_rules! check {($ok:expr,$why:literal)=>{if !$ok {return Err($why.into())}}}
            let owner:i64=sqlx::query_scalar("INSERT INTO users(email,status,password) VALUES('managed-owner@test','activated','unused') RETURNING id").fetch_one(&pool).await?;
            let actor:i64=sqlx::query_scalar("INSERT INTO users(email,status,password) VALUES('managed-actor@test','activated','unused') RETURNING id").fetch_one(&pool).await?;
            let project:i64=sqlx::query_scalar("INSERT INTO projects(name,status,profiles) VALUES('Managed profile','active','{}') RETURNING id").fetch_one(&pool).await?;
            sqlx::query("INSERT INTO user_projects(user_id,project_id,is_owner) VALUES($1,$2,TRUE),($3,$2,FALSE)").bind(owner).bind(project).bind(actor).execute(&pool).await?;
            let model:i64=sqlx::query_scalar("INSERT INTO models(developer,model_id,type,name,icon,\"group\",model_card,settings,status) VALUES('test','managed-public','chat','Managed','','test','{}','{}','enabled') RETURNING id").fetch_one(&pool).await?;
            let channel:i64=sqlx::query_scalar("INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) VALUES('openai','Managed channel','enabled','{}','[]','') RETURNING id").fetch_one(&pool).await?;
            let deployment:i64=sqlx::query_scalar("INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,status) VALUES($1,'managed-upstream','managed-upstream','enabled') RETURNING id").bind(channel).fetch_one(&pool).await?;
            sqlx::query("INSERT INTO model_routes(public_model_id,deployment_id,status) VALUES($1,$2,'enabled')").bind(model).bind(deployment).execute(&pool).await?;
            sqlx::query("INSERT INTO project_entitlement_overrides(project_id,public_model_id,effect,source_type,status,created_at,updated_at) VALUES($1,$2,'allow','test','active',now(),now())").bind(project).bind(model).execute(&pool).await?;
            let adapter=PgGroupAdapter::new(pool.clone());
            let group=adapter.save_model_group(SaveModelGroupInput{id:None,name:"Owner models".into(),description:None,status:GroupStatus::Enabled,model_ids:vec![ID("managed-public".into())]}).await?;
            let ug=adapter.save_user_group(SaveUserGroupInput{recharge_multiplier_ppm:None,expected_recharge_revision:None,id:None,name:"Only target owner".into(),description:None,status:GroupStatus::Enabled,member_user_ids:vec![ID(owner.to_string())],model_group_ids:vec![group.id.clone()]}).await?;
            let repo=PgApiKeyRepo::new(pool.clone());let ctx=RequestContext::new(PolicyContext::new(Principal::test()));
            let key=repo.create_api_key(&ctx,CreateApiKeyInput{model_group_id:Some(group.id.to_string()),id:String::new(),user_id:Some(owner.to_string()),project_id:project.to_string(),name:"managed-key".into(),key:"synthetic-managed-key".into(),key_type:"user".into(),scopes:vec![],profiles:None,created_at:chrono::Utc::now().to_rfc3339()}).await?;
            let key_id=key.id.parse::<i64>()?;
            let prospective=adapter.creation_catalog(Some(owner),None,project,group.id.as_str()).await?;
            check!(prospective.configured&&prospective.key_id.is_none(),"prospective owner candidates missing");
            check!(!adapter.creation_catalog(Some(actor),None,project,group.id.as_str()).await?.configured,"prospective actor borrowed another owner's grants");
            check!(!adapter.creation_catalog(Some(owner),None,project+999,group.id.as_str()).await?.configured,"prospective cross-Project candidates leaked");
            check!(adapter.creation_catalog(None,None,project,group.id.as_str()).await.is_err(),"missing creator identity admitted");
            check!(adapter.creation_catalog(None,Some(key_id),project,group.id.as_str()).await?.configured,"delegated owner candidates missing");
            check!(adapter.creation_catalog(None,Some(key_id),project+999,group.id.as_str()).await.is_err(),"cross-Project delegation admitted");
            // Administrative editing must allow restoring a disabled Key and expanding its profile
            // inside the actual owner's group, even though the actor has no UserGroup at all.
            sqlx::query("UPDATE api_keys SET status='disabled',profiles=$1 WHERE id=$2").bind(serde_json::json!({"activeProfile":"narrow","profiles":[{"name":"narrow","modelIDs":["not-in-group"],"channelIDs":[999999]}]})).bind(key_id).execute(&pool).await?;
            let data=adapter.profile_catalog(key_id,project).await?;
            check!(data.configured,"target owner candidates missing");
            check!(data.billing_user_id.as_ref().map(|v|v.as_str())==Some(owner.to_string().as_str()),"actor substituted for billing owner");
            check!(data.model_ids==vec!["managed-public"],"profile hid legitimate group expansion");
            check!(data.channels.iter().any(|c|c.id.as_str()==channel.to_string()&&c.name=="Managed channel"),"channel metadata missing");
            check!(adapter.profile_catalog(key_id,project+999).await.is_err(),"cross-Project Key leaked");
            let services:std::sync::Arc<dyn GroupServices>=std::sync::Arc::new(adapter.clone());
            let schema=conduit_admin_graphql::admin_schema_builder().extension(conduit_admin_graphql::authz_extension::ScopeAuthExtensionFactory).data(services).data(std::sync::Arc::new(crate::wiring_governance::PgGovernanceAdapter::new(pool.clone())) as std::sync::Arc<dyn conduit_admin_graphql::governance::GovernanceServices>).finish();
            let query=format!("{{apiKeyProfileCatalog(keyID:\"gid://conduit/APIKey/{key_id}\"){{billingUserID modelGroupID configured modelIDs channels{{id name}}}}}}");
            let denied=schema.execute(query.as_str()).await;
            check!(!denied.errors.is_empty(),"anonymous management query admitted");
            check!(!denied.data.to_string().contains("Managed channel"),"anonymous metadata leaked");
            let mut principal=conduit_auth::RequestContext::new();
            principal.set_principal(conduit_auth::Principal::user(actor.to_string()).with_scope(conduit_auth::Scope::project(project.to_string(),conduit_auth::scopes::slug::READ_API_KEYS)))?;
            principal.set_project_id(project.to_string())?;
            let allowed=schema.execute(async_graphql::Request::new(&query).data(principal)).await;
            check!(allowed.errors.is_empty(),"authorized management actor rejected");
            check!(allowed.data.to_string().contains("Managed channel"),"authorized owner metadata unavailable");
            let create_query=format!("{{apiKeyCreationCatalog(modelGroupID:\"{}\"){{keyID billingUserID configured modelIDs channels{{id name}}}}}}",group.id.as_str());
            check!(!schema.execute(create_query.as_str()).await.errors.is_empty(),"anonymous prospective metadata admitted");
            let mut creator=conduit_auth::RequestContext::new();
            creator.set_principal(conduit_auth::Principal::user(owner.to_string()).with_scope(conduit_auth::Scope::project(project.to_string(),conduit_auth::scopes::slug::WRITE_API_KEYS)))?;
            creator.set_project_id(project.to_string())?;
            let created=schema.execute(async_graphql::Request::new(&create_query).data(creator).data(conduit_admin_graphql::me::CurrentUser{user_id:owner})).await;
            check!(created.errors.is_empty()&&created.data.to_string().contains("Managed channel"),"trusted creator metadata unavailable");
            // Registered production root gate plus real resolver and PostgreSQL adapter.
            let global_ops = vec![
                ("{modelGroups{id}}".to_string(), conduit_auth::scopes::slug::READ_USERS),
                ("{userGroups{id}}".to_string(), conduit_auth::scopes::slug::READ_USERS),
                (format!(r#"mutation{{saveModelGroup(input:{{id:"{}",name:"Owner models",status:ENABLED,modelIDs:["managed-public"]}}){{id}}}}"#,group.id.as_str()), conduit_auth::scopes::slug::WRITE_USERS),
                (format!(r#"mutation{{saveUserGroup(input:{{id:"{}",name:"Only target owner",status:ENABLED,memberUserIDs:["{owner}"],modelGroupIDs:["{}"]}}){{id}}}}"#,ug.id.as_str(),group.id.as_str()), conduit_auth::scopes::slug::WRITE_USERS),
                (format!(r#"mutation{{moveUserToGroup(userID:"{owner}",userGroupID:"{}")}}"#,ug.id.as_str()), conduit_auth::scopes::slug::WRITE_USERS),
            ];
            let key_ops = vec![
                (format!(r#"{{apiKeyAssignableModelGroups(keyID:"{key_id}"){{id}}}}"#),conduit_auth::scopes::slug::READ_API_KEYS),
                (query.clone(),conduit_auth::scopes::slug::READ_API_KEYS),
                (create_query.clone(),conduit_auth::scopes::slug::WRITE_API_KEYS),
                (format!(r#"mutation{{assignApiKeyModelGroup(id:"{key_id}",modelGroupID:"{}")}}"#,group.id.as_str()),conduit_auth::scopes::slug::WRITE_API_KEYS),
            ];
            for (operation,scope) in global_ops.iter().chain(key_ops.iter()) {
                for principal in [
                    conduit_auth::Principal::user(owner.to_string()).with_owner(true),
                    conduit_auth::Principal::user(owner.to_string()).with_scope(*scope),
                    conduit_auth::Principal::user(owner.to_string()).with_scope(conduit_auth::Scope::system_role(*scope)),
                ] {
                    let mut rc=conduit_auth::RequestContext::new();rc.set_principal(principal)?;rc.set_project_id(project.to_string())?;
                    let response=schema.execute(async_graphql::Request::new(operation).data(rc).data(conduit_admin_graphql::me::CurrentUser{user_id:owner})).await;
                    if !response.errors.is_empty(){return Err(format!("registered authorized {operation}: {:?}",response.errors).into())}
                }
                check!(!schema.execute(operation.as_str()).await.errors.is_empty(),"registered anonymous operation admitted");
                let mut rc=conduit_auth::RequestContext::new();rc.set_principal(conduit_auth::Principal::user(actor.to_string()))?;rc.set_project_id(project.to_string())?;
                check!(!schema.execute(async_graphql::Request::new(operation).data(rc).data(conduit_admin_graphql::me::CurrentUser{user_id:actor})).await.errors.is_empty(),"ordinary user admitted");
            }
            for (operation,_) in &global_ops {
                let mut rc=conduit_auth::RequestContext::new();rc.set_principal(conduit_auth::Principal::user(actor.to_string()).with_scope(conduit_auth::Scope::project_membership(project.to_string(),conduit_auth::scopes::slug::WILDCARD)))?;rc.set_project_id(project.to_string())?;
                check!(!schema.execute(async_graphql::Request::new(operation).data(rc)).await.errors.is_empty(),"Project admin gained global group authority");
            }
            for (operation,scope) in &key_ops {
                for (granted_project,expected) in [(project,true),(project+999,false)] {
                    let mut rc=conduit_auth::RequestContext::new();rc.set_principal(conduit_auth::Principal::user(owner.to_string()).with_scope(conduit_auth::Scope::project_role(granted_project.to_string(),*scope)))?;rc.set_project_id(project.to_string())?;
                    let response=schema.execute(async_graphql::Request::new(operation).data(rc).data(conduit_admin_graphql::me::CurrentUser{user_id:owner})).await;
                    if response.errors.is_empty()!=expected{return Err(format!("registered Project boundary {operation}: {:?}",response.errors).into())}
                }
            }
            let mut policy=crate::wiring_governance::Policy::default();policy.review_kinds.push("group_access".into());
            sqlx::query("INSERT INTO systems(key,value,created_at,updated_at) VALUES($1,$2,now(),now()) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value").bind(crate::wiring_governance::KEY).bind(serde_json::to_string(&policy)?).execute(&pool).await?;
            for (operation,_) in global_ops.iter().skip(2) {
                let mut rc=conduit_auth::RequestContext::new();rc.set_principal(conduit_auth::Principal::user(owner.to_string()).with_owner(true))?;rc.set_project_id(project.to_string())?;
                let response=schema.execute(async_graphql::Request::new(operation).data(rc).data(conduit_admin_graphql::me::CurrentUser{user_id:owner})).await;
                check!(response.errors.iter().any(|error|error.message.starts_with("APPROVAL_REQUIRED")),"registered group write bypassed real governance policy");
            }
            let shape=schema.execute("{__type(name:\"APIKeyProfileCatalog\"){fields{name}}}").await.data.to_string();
            check!(!shape.to_lowercase().contains("price"),"management DTO exposed pricing");
            sqlx::query("DELETE FROM user_group_model_groups WHERE user_group_id=$1").bind(ug.id.as_str()).execute(&pool).await?;
            let revoked=adapter.profile_catalog(key_id,project).await?;
            check!(!revoked.configured&&revoked.model_ids.is_empty()&&revoked.channels.is_empty(),"revoked metadata remained available");
            Ok(())
        }.await;
        isolated.cleanup().await?;
        result
    }
}

const DEFAULT_CONFLICT_KEY: &str = "system_default_model_group_conflict";

pub(crate) async fn initialize_default_model_group(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(0x434f_4e44_4d47_0001_i64)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO model_groups(id,name,status) SELECT $1,'Default','enabled' WHERE NOT EXISTS (SELECT 1 FROM model_groups WHERE id=$1) ON CONFLICT DO NOTHING")
        .bind(DEFAULT_MODEL_GROUP_ID).execute(&mut *tx).await?;
    let initialized =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM model_groups WHERE id=$1)")
            .bind(DEFAULT_MODEL_GROUP_ID)
            .fetch_one(&mut *tx)
            .await?;
    if initialized {
        sqlx::query("DELETE FROM systems WHERE key=$1 AND deleted_at=0")
            .bind(DEFAULT_CONFLICT_KEY)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("INSERT INTO systems(key,value) VALUES($1,'DEFAULT_MODEL_GROUP_CONFLICT') ON CONFLICT(key) DO UPDATE SET deleted_at=0 WHERE systems.deleted_at<>0")
            .bind(DEFAULT_CONFLICT_KEY).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    if !initialized {
        tracing::warn!(
            code = "DEFAULT_MODEL_GROUP_CONFLICT",
            "reserved Default model group could not be initialized because its name is occupied"
        );
    }
    Ok(())
}
