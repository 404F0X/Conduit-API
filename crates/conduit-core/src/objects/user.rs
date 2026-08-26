//! User-facing info objects ported from `conduit/internal/objects/user.go`.

use serde::{Deserialize, Serialize};

// TODO(parity): `GUID` is a Go named type defined in a shared file not yet
// ported; represented as `String` (round-trips the JSON string value
// faithfully). Replace with a typed GUID once that file lands.
type Guid = String;

/// Ported 1:1 from Go `RoleInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RoleInfo {
    #[serde(default)]
    pub name: String,
}

/// Ported 1:1 from Go `OIDCIdentityInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OidcIdentityInfo {
    #[serde(default)]
    pub id: Guid,
    #[serde(default)]
    pub idp_name: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub email: String,
}

/// Ported 1:1 from Go `UserProjectInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserProjectInfo {
    #[serde(default, rename = "projectID")]
    pub project_id: Guid,
    #[serde(default)]
    pub is_owner: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RoleInfo>,
}

/// Ported 1:1 from Go `UserInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserInfo {
    #[serde(default)]
    pub id: Guid,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub last_name: String,
    #[serde(default)]
    pub is_owner: bool,
    #[serde(default)]
    pub prefer_language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RoleInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<UserProjectInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oidc_identities: Vec<OidcIdentityInfo>,
    #[serde(default)]
    pub has_password: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_info_round_trip() -> Result<(), serde_json::Error> {
        let input = r#"{"id":"u-1","email":"a@b.c","firstName":"Ada","lastName":"Lovelace","isOwner":true,"preferLanguage":"en","avatar":"https://x/y.png","scopes":["read"],"roles":[{"name":"admin"}],"projects":[{"projectID":"p-1","isOwner":true,"scopes":["read"],"roles":[{"name":"owner"}]}],"oidcIdentities":[{"id":"o-1","idpName":"google","issuer":"https://g","subject":"sub","email":"a@b.c"}],"hasPassword":true}"#;
        let user: UserInfo = serde_json::from_str(input)?;
        assert_eq!(user.email, "a@b.c");
        assert!(user.is_owner);
        assert_eq!(user.avatar.as_deref(), Some("https://x/y.png"));
        assert_eq!(user.roles.len(), 1);
        assert_eq!(user.projects.len(), 1);
        assert_eq!(user.projects[0].project_id, "p-1");
        assert_eq!(user.oidc_identities.len(), 1);
        assert_eq!(user.oidc_identities[0].idp_name, "google");

        // Round-trip preserves the exact `projectID` / camelCase shape.
        let re = serde_json::to_value(&user)?;
        assert!(re.get("projectID").is_none()); // top-level has no projectID
        let proj = re
            .get("projects")
            .and_then(|v| v.get(0))
            .and_then(|v| v.get("projectID"))
            .and_then(|v| v.as_str());
        assert_eq!(proj, Some("p-1"));
        assert_eq!(
            re.get("oidcIdentities")
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("idpName"))
                .and_then(|v| v.as_str()),
            Some("google")
        );
        // omitempty: a user with no avatar/roles round-trips without those keys.
        let minimal: UserInfo = serde_json::from_str(
            r#"{"id":"u-2","email":"x@y.z","firstName":"","lastName":"","isOwner":false,"preferLanguage":"","scopes":[],"roles":[],"projects":[],"oidcIdentities":[],"hasPassword":false}"#,
        )?;
        let min = serde_json::to_value(&minimal)?;
        assert!(min.get("avatar").is_none());
        assert!(min.get("roles").is_none());
        Ok(())
    }
}
