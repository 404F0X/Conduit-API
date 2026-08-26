use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_JWT_TTL: Duration = Duration::days(7);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Claims {
    /// Numeric user identifier, mirroring Go's `jwt.MapClaims{"user_id": user.ID}`
    /// where `user.ID` is a Go `int` (conduit/internal/server/biz/auth.go:108-109).
    /// Go decodes it back as a JSON number (`float64`, auth.go:187), so the JWT
    /// payload carries `user_id` as a JSON number — NOT a string. Keeping this
    /// field `i64` ensures a token issued by the Go binary deserializes cleanly.
    pub user_id: i64,
    pub session_scope: String,
    pub exp: i64,
    pub iat: i64,
}

#[derive(Debug, Error)]
pub enum JwtError {
    #[error("jwt failed: {0}")]
    JsonWebToken(#[from] jsonwebtoken::errors::Error),
}

impl Claims {
    pub fn new(user_id: i64, session_scope: impl Into<String>) -> Self {
        Self::with_ttl(user_id, session_scope, DEFAULT_JWT_TTL)
    }

    pub fn with_ttl(user_id: i64, session_scope: impl Into<String>, ttl: Duration) -> Self {
        let now = Utc::now();
        let exp = now + ttl;
        Self {
            user_id,
            session_scope: session_scope.into(),
            // NumericDate values are seconds since Unix epoch for jsonwebtoken validation.
            exp: exp.timestamp(),
            iat: now.timestamp(),
        }
    }
}

pub fn encode_hs256(claims: &Claims, secret: impl AsRef<[u8]>) -> Result<String, JwtError> {
    let header = Header::new(Algorithm::HS256);
    Ok(jsonwebtoken::encode(
        &header,
        claims,
        &EncodingKey::from_secret(secret.as_ref()),
    )?)
}

pub fn decode_hs256(token: &str, secret: impl AsRef<[u8]>) -> Result<Claims, JwtError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    let data = jsonwebtoken::decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_ref()),
        &validation,
    )?;
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hs256_token_round_trips_claims() -> Result<(), JwtError> {
        let claims = Claims::new(1, "session:project:project-1");
        let token = encode_hs256(&claims, "secret")?;
        let decoded = decode_hs256(&token, "secret")?;

        assert_eq!(decoded.user_id, 1);
        assert_eq!(decoded.session_scope, "session:project:project-1");
        assert_eq!(decoded.exp, claims.exp);
        assert_eq!(decoded.iat, claims.iat);
        Ok(())
    }

    #[test]
    fn default_expiration_is_seven_days() {
        let claims = Claims::new(1, "session:all");
        assert_eq!(claims.exp - claims.iat, DEFAULT_JWT_TTL.num_seconds());
    }

    /// Parity proof: a JWT issued by the Go binary carries `user_id` as a JSON
    /// number (Go `jwt.MapClaims{"user_id": user.ID}` with `user.ID` of type
    /// `int`, auth.go:108-109; decoded back as `float64`, auth.go:187). This
    /// test builds a token whose payload is exactly the Go shape — a
    /// `serde_json::Value` with `user_id` as a JSON number — signs it HS256 via
    /// `jsonwebtoken::encode`, and asserts `decode_hs256` recovers the numeric
    /// `user_id`. Before the fix (`user_id: String`) this token would fail to
    /// deserialize because serde cannot coerce a JSON number into `String`.
    ///
    /// Using `serde_json::Value` as the encode claims type (rather than `Claims`
    /// itself) guarantees the wire payload is a true JSON number — not an
    /// artifact of `Claims`'s own `Serialize` — so this is a faithful mirror of
    /// what the Go binary emits.
    #[test]
    fn go_style_numeric_user_id_token_decodes() -> Result<(), Box<dyn std::error::Error>> {
        use chrono::Utc;
        use jsonwebtoken::{Algorithm, EncodingKey, Header};
        use serde_json::json;

        let now = Utc::now();
        let payload = json!({
            // Numeric `user_id` — exactly what the Go binary emits.
            "user_id": 42_i64,
            "session_scope": "user:42",
            "exp": (now + Duration::days(1)).timestamp(),
            "iat": now.timestamp(),
        });

        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &payload,
            &EncodingKey::from_secret(b"go-style-secret"),
        )?;

        let decoded = decode_hs256(&token, "go-style-secret")?;
        assert_eq!(decoded.user_id, 42);
        assert_eq!(decoded.session_scope, "user:42");
        Ok(())
    }
}
