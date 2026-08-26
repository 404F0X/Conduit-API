//! Test-only in-memory implementations of the OpenAPI service traits.
//!
//! Mirrors the fixture environment of Go
//! `internal/server/gql/openapi/openapi_test.go::setupOpenAPI` — real biz
//! semantics (scope checks, project filtering, exactly-one-of, duplicate-name
//! rejection, append-only template loading) over an in-memory store, so the
//! resolver tests exercise the same observable behaviour as the Go suite
//! without a database. Production implementations live with the services
//! crate; nothing here is compiled outside `cfg(test)`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use conduit_auth::Principal;
use conduit_auth::scopes::slug;
use conduit_core::{ConduitError, ErrorKind};

use crate::model::{APIKeyProfile, APIKeyProfiles, APIKeyQuota, APIKeyQuotaPeriodType};
use crate::service::{
    ApiKeyProfileTemplateRecord, ApiKeyRecord, OpenApiApiKeyProfileTemplateService,
    OpenApiApiKeyService, OpenApiQuotaService, OpenApiServices, ProfileQuotaUsage, QuotaUsage,
    QuotaWindow,
};

// One stored API key row (the slice of ent.APIKey the mock needs).
#[derive(Debug, Clone)]
pub(crate) struct StoredKey {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub scopes: Option<Vec<String>>,
    pub profiles: Option<APIKeyProfiles>,
    pub project_id: String,
}

// One stored template row.
#[derive(Debug, Clone)]
pub(crate) struct StoredTemplate {
    pub id: i64,
    pub name: String,
    pub project_id: String,
    pub profile: Option<APIKeyProfile>,
}

// Seeded usage aggregate for one key (stands in for the usage_log rows the Go
// quota service sums).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SeededUsage {
    pub request_count: i64,
    pub total_tokens: i64,
    pub total_cost: rust_decimal::Decimal,
}

#[derive(Debug, Default)]
struct State {
    keys: Vec<StoredKey>,
    templates: Vec<StoredTemplate>,
    usage: std::collections::BTreeMap<i64, SeededUsage>,
    next_id: i64,
}

/// In-memory backing store implementing all three OpenAPI service traits.
pub(crate) struct InMemoryOpenApi {
    state: Mutex<State>,
}

// Locks are held only across synchronous sections (never across an await), so
// a std Mutex suffices. Poisoning is surfaced as an internal error instead of
// panicking (workspace forbids unwrap/expect).
fn lock_err() -> ConduitError {
    ConduitError::new(ErrorKind::Internal, "in-memory state poisoned")
}

// Scope check mirroring the ent privacy Denyf text
// (`internal/scopes/rule_apikey_scope.go:33`): "API key does not have required
// scope: <scope>".
fn require_scope(caller: &Principal, scope: &str) -> Result<(), ConduitError> {
    if caller.scopes.contains(scope) {
        return Ok(());
    }
    Err(ConduitError::new(
        ErrorKind::Forbidden,
        format!("API key does not have required scope: {scope}"),
    ))
}

// The caller's project — WithOpenAPIAuth always injects it for service
// accounts; a missing project is a wiring bug we refuse to widen.
fn caller_project(caller: &Principal) -> Result<String, ConduitError> {
    caller
        .project_id
        .clone()
        .ok_or_else(|| ConduitError::new(ErrorKind::Forbidden, "principal lacks a project_id"))
}

fn record_of(k: &StoredKey) -> ApiKeyRecord {
    ApiKeyRecord {
        id: k.id,
        key: k.key.clone(),
        name: k.name.clone(),
        scopes: k.scopes.clone(),
        profiles: k.profiles.clone(),
        project_id: k.project_id.clone(),
    }
}

// Verbatim port of Go `resolveProfileNameConflict`
// (`biz/api_key_profile_template.go:235-251`): first free "<name>" or
// "<name> (i)" starting at i=1.
fn resolve_profile_name_conflict(existing: &[APIKeyProfile], new_name: &str) -> String {
    let taken: std::collections::BTreeSet<&str> =
        existing.iter().map(|p| p.name.as_str()).collect();
    if !taken.contains(new_name) {
        return new_name.to_string();
    }
    let mut i: i64 = 1;
    loop {
        let candidate = format!("{new_name} ({i})");
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
        i += 1;
    }
}

impl InMemoryOpenApi {
    // Seed a quota-bearing "Default" profile onto a key — mirrors the Go test
    // helper `setKeyQuotaProfile` (requests=100, totalTokens=1000, all_time).
    pub(crate) fn set_key_quota_profile(&self, key_id: i64) -> Result<(), ConduitError> {
        let mut state = self.state.lock().map_err(|_| lock_err())?;
        let key = state
            .keys
            .iter_mut()
            .find(|k| k.id == key_id)
            .ok_or_else(|| ConduitError::new(ErrorKind::NotFound, "api_key not found"))?;
        key.profiles = Some(APIKeyProfiles {
            active_profile: "Default".to_string(),
            profiles: Some(vec![APIKeyProfile {
                name: "Default".to_string(),
                quota: Some(APIKeyQuota {
                    requests: Some(100),
                    total_tokens: Some(1000),
                    cost: None,
                    period: crate::model::APIKeyQuotaPeriod {
                        r#type: APIKeyQuotaPeriodType::AllTime,
                        past_duration: None,
                        calendar_duration: None,
                    },
                }),
                ..APIKeyProfile::default()
            }]),
        });
        Ok(())
    }

    // Seed a usage aggregate for a key — stands in for the usage_log rows the
    // e2e suite inserts (requestCount=2, totalTokens=300, totalCost=2).
    pub(crate) fn seed_usage(&self, key_id: i64, usage: SeededUsage) -> Result<(), ConduitError> {
        let mut state = self.state.lock().map_err(|_| lock_err())?;
        state.usage.insert(key_id, usage);
        Ok(())
    }
}

#[async_trait]
impl OpenApiApiKeyService for InMemoryOpenApi {
    async fn create_llm_api_key(
        &self,
        caller: &Principal,
        name: &str,
    ) -> Result<ApiKeyRecord, ConduitError> {
        // Go order (`biz/api_key.go:228-232`): trim + required-name check runs
        // before the privacy mutation policy (which fires at Save).
        let name = name.trim();
        if name.is_empty() {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "api key name is required",
            ));
        }

        require_scope(caller, slug::WRITE_API_KEYS)?;
        let project = caller_project(caller)?;

        let mut state = self.state.lock().map_err(|_| lock_err())?;

        // Per-project name uniqueness (`biz/api_key.go:283-295`, wire text
        // from `xerrors.DuplicateNameError`).
        if state
            .keys
            .iter()
            .any(|k| k.project_id == project && k.name == name)
        {
            return Err(ConduitError::new(
                ErrorKind::Conflict,
                format!("API Key name '{name}' already exists"),
            ));
        }

        state.next_id += 1;
        let id = state.next_id;
        let stored = StoredKey {
            id,
            key: format!("conduit-mock-{id}"),
            name: name.to_string(),
            // Fixed scope set from `biz/api_key.go:261-264`.
            scopes: Some(vec![
                slug::READ_CHANNELS.to_string(),
                slug::WRITE_REQUESTS.to_string(),
            ]),
            profiles: None,
            project_id: project,
        };
        let record = record_of(&stored);
        state.keys.push(stored);
        Ok(record)
    }

    async fn get_for_read(
        &self,
        caller: &Principal,
        id: Option<i64>,
        key: Option<&str>,
        name: Option<&str>,
    ) -> Result<ApiKeyRecord, ConduitError> {
        // Exactly-one-of first, mirroring `biz/api_key.go:708-710`.
        let provided =
            usize::from(id.is_some()) + usize::from(key.is_some()) + usize::from(name.is_some());
        if provided != 1 {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "exactly one of api key id, key, or name must be provided",
            ));
        }

        // Privacy read rule: read_api_keys + own-project filter
        // (`scopes.APIKeyProjectScopeReadRule`).
        require_scope(caller, slug::READ_API_KEYS)?;
        let project = caller_project(caller)?;

        let state = self.state.lock().map_err(|_| lock_err())?;
        state
            .keys
            .iter()
            .filter(|k| k.project_id == project)
            .find(|k| {
                id.is_some_and(|v| k.id == v)
                    || key.is_some_and(|v| k.key == v)
                    || name.is_some_and(|v| k.name == v)
            })
            .map(record_of)
            // Uniform NotFound (Go surfaces ent's "not found"): foreign or
            // missing keys are indistinguishable — no existence leak.
            .ok_or_else(|| ConduitError::new(ErrorKind::NotFound, "api_key not found"))
    }

    async fn update_api_key_profiles(
        &self,
        caller: &Principal,
        id: i64,
        profiles: APIKeyProfiles,
    ) -> Result<ApiKeyRecord, ConduitError> {
        // Privacy mutation rule: write_api_keys + project-bounded row filter
        // (`scopes.APIKeyProjectScopeWriteRule`).
        require_scope(caller, slug::WRITE_API_KEYS)?;
        let project = caller_project(caller)?;

        let mut state = self.state.lock().map_err(|_| lock_err())?;
        let key = state
            .keys
            .iter_mut()
            .find(|k| k.id == id && k.project_id == project)
            .ok_or_else(|| ConduitError::new(ErrorKind::NotFound, "api_key not found"))?;
        key.profiles = Some(profiles);
        Ok(record_of(key))
    }
}

#[async_trait]
impl OpenApiApiKeyProfileTemplateService for InMemoryOpenApi {
    async fn get_for_read(
        &self,
        caller: &Principal,
        id: Option<i64>,
        name: Option<&str>,
    ) -> Result<ApiKeyProfileTemplateRecord, ConduitError> {
        // Exactly-one-of (`biz/api_key_profile_template.go:79-82`).
        if id.is_some() == name.is_some() {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "exactly one of template id or name must be provided",
            ));
        }

        require_scope(caller, slug::READ_API_KEYS)?;
        let project = caller_project(caller)?;

        let state = self.state.lock().map_err(|_| lock_err())?;
        state
            .templates
            .iter()
            .filter(|t| t.project_id == project)
            .find(|t| id.is_some_and(|v| t.id == v) || name.is_some_and(|v| t.name == v))
            .map(|t| ApiKeyProfileTemplateRecord {
                id: t.id,
                name: t.name.clone(),
                project_id: t.project_id.clone(),
                profile: t.profile.clone(),
            })
            .ok_or_else(|| {
                ConduitError::new(ErrorKind::NotFound, "api_key_profile_template not found")
            })
    }

    async fn load_template(
        &self,
        caller: &Principal,
        template_id: i64,
        api_key_id: i64,
    ) -> Result<ApiKeyRecord, ConduitError> {
        // The Save inside Go's transaction runs under the write mutation rule;
        // the inner Gets run under the read rules with the project filter.
        require_scope(caller, slug::WRITE_API_KEYS)?;
        let project = caller_project(caller)?;

        let mut state = self.state.lock().map_err(|_| lock_err())?;

        let template = state
            .templates
            .iter()
            .find(|t| t.id == template_id && t.project_id == project)
            .cloned()
            .ok_or_else(|| {
                ConduitError::new(ErrorKind::NotFound, "api_key_profile_template not found")
            })?;

        let key = state
            .keys
            .iter_mut()
            .find(|k| k.id == api_key_id && k.project_id == project)
            .ok_or_else(|| ConduitError::new(ErrorKind::NotFound, "api_key not found"))?;

        // Same-project guard verbatim from `LoadTemplate` (go:196-198) — kept
        // even though the project filters above already imply it.
        if template.project_id != key.project_id {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "template and API key must belong to the same project",
            ));
        }

        // Clone template profile (go:200-203).
        let mut profile = template.profile.clone().ok_or_else(|| {
            ConduitError::new(ErrorKind::InvalidRequest, "template has no profile")
        })?;

        // Nil profiles → empty container (go:205-208), activeProfile untouched.
        let mut profiles = key.profiles.clone().unwrap_or_default();

        // Profile-name fallback + conflict resolution (go:210-215).
        let base_name = if profile.name.is_empty() {
            template.name.clone()
        } else {
            profile.name.clone()
        };
        let existing = profiles.profiles.get_or_insert_with(Vec::new);
        profile.name = resolve_profile_name_conflict(existing, &base_name);

        // Append-only (go:217).
        existing.push(profile);
        key.profiles = Some(profiles);

        Ok(record_of(key))
    }
}

#[async_trait]
impl OpenApiQuotaService for InMemoryOpenApi {
    async fn profile_quota_usages(
        &self,
        _caller: &Principal,
        api_key: &ApiKeyRecord,
    ) -> Result<Vec<ProfileQuotaUsage>, ConduitError> {
        // Go returns nil for a key with no profiles (`quota.go:130-132`); the
        // resolver still renders `[]` because it builds its own slice.
        let Some(profiles) = &api_key.profiles else {
            return Ok(Vec::new());
        };
        let Some(entries) = &profiles.profiles else {
            return Ok(Vec::new());
        };

        let state = self.state.lock().map_err(|_| lock_err())?;
        let seeded = state.usage.get(&api_key.id).copied().unwrap_or_default();

        let mut out = Vec::new();
        for profile in entries {
            let Some(quota) = &profile.quota else {
                // Profiles without quota are skipped (`quota.go:137-139`).
                continue;
            };

            // Window semantics: all_time = open start, end = now
            // (`quota.go:quotaWindow`). The Go tests on this surface only
            // exercise all_time; the full past/calendar window math ports with
            // the quota service itself, so other period types reuse the same
            // simplified open-start window here (test-only stand-in).
            let window = match quota.period.r#type {
                APIKeyQuotaPeriodType::AllTime
                | APIKeyQuotaPeriodType::PastDuration
                | APIKeyQuotaPeriodType::CalendarDuration => QuotaWindow {
                    start: None,
                    end: Some(Utc::now()),
                },
            };

            out.push(ProfileQuotaUsage {
                profile_name: profile.name.clone(),
                quota: *quota,
                window,
                usage: QuotaUsage {
                    request_count: seeded.request_count,
                    total_tokens: seeded.total_tokens,
                    total_cost: seeded.total_cost,
                },
            });
        }
        Ok(out)
    }
}

/// Fixture ids/names shared by the resolver tests — mirrors the `fixtures`
/// struct in Go `openapi_test.go:29-39`.
pub(crate) struct Fixtures {
    pub target_key_id: i64,
    pub target_key: String,
    pub target_key_name: String,
    pub template_id: i64,
    pub template_name: String,
    pub other_template_id: i64,
    pub other_template_name: String,
    pub other_key_id: i64,
    pub other_key: String,
    pub other_key_name: String,
}

/// Everything a resolver test needs: the concrete mock (for seeding), the
/// trait-object bundle, the fixture ids and the caller principal.
pub(crate) struct TestEnv {
    pub mem: Arc<InMemoryOpenApi>,
    pub services: OpenApiServices,
    pub fx: Fixtures,
    pub principal: Principal,
}

/// Build the Go `setupOpenAPI` fixture set: a service-account caller in
/// project "proj-1" with the given scopes, a target key with a bare "Default"
/// profile, a "prod-template" carrying a "Production" profile, plus a foreign
/// project's template and key for the cross-project denial tests.
pub(crate) fn fixture(service_account_scopes: &[&str]) -> TestEnv {
    let production_profile = APIKeyProfile {
        name: "Production".to_string(),
        model_mappings: Some(vec![crate::model::ModelMapping {
            from: "claude-3".to_string(),
            to: "claude-3-opus".to_string(),
        }]),
        ..APIKeyProfile::default()
    };

    let state = State {
        keys: vec![
            // The service-account key itself (id 1) — the caller.
            StoredKey {
                id: 1,
                key: "conduit-mock-sa".to_string(),
                name: "service-account".to_string(),
                scopes: Some(
                    service_account_scopes
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect(),
                ),
                profiles: None,
                project_id: "proj-1".to_string(),
            },
            // The target user key with a bare Default profile.
            StoredKey {
                id: 2,
                key: "conduit-mock-target".to_string(),
                name: "target-llm-key".to_string(),
                scopes: None,
                profiles: Some(APIKeyProfiles {
                    active_profile: "Default".to_string(),
                    profiles: Some(vec![APIKeyProfile {
                        name: "Default".to_string(),
                        ..APIKeyProfile::default()
                    }]),
                }),
                project_id: "proj-1".to_string(),
            },
            // A key in a foreign project (invisible to the caller).
            StoredKey {
                id: 5,
                key: "conduit-mock-foreign".to_string(),
                name: "foreign-key".to_string(),
                scopes: None,
                profiles: None,
                project_id: "proj-2".to_string(),
            },
        ],
        templates: vec![
            StoredTemplate {
                id: 3,
                name: "prod-template".to_string(),
                project_id: "proj-1".to_string(),
                profile: Some(production_profile),
            },
            StoredTemplate {
                id: 4,
                name: "other-template".to_string(),
                project_id: "proj-2".to_string(),
                profile: Some(APIKeyProfile {
                    name: "ForeignProfile".to_string(),
                    ..APIKeyProfile::default()
                }),
            },
        ],
        usage: std::collections::BTreeMap::new(),
        next_id: 100,
    };

    let mem = Arc::new(InMemoryOpenApi {
        state: Mutex::new(state),
    });

    let services = OpenApiServices {
        api_keys: mem.clone(),
        templates: mem.clone(),
        quota: mem.clone(),
    };

    // The principal WithOpenAPIAuth would inject: a service-account API key
    // bound to its project, carrying the key's scopes.
    let mut principal = Principal::api_key_service_account("1", "proj-1");
    for scope in service_account_scopes {
        principal = principal.with_scope(*scope);
    }

    TestEnv {
        mem,
        services,
        fx: Fixtures {
            target_key_id: 2,
            target_key: "conduit-mock-target".to_string(),
            target_key_name: "target-llm-key".to_string(),
            template_id: 3,
            template_name: "prod-template".to_string(),
            other_template_id: 4,
            other_template_name: "other-template".to_string(),
            other_key_id: 5,
            other_key: "conduit-mock-foreign".to_string(),
            other_key_name: "foreign-key".to_string(),
        },
        principal,
    }
}
