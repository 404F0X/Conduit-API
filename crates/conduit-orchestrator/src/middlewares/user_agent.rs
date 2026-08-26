//! Pipeline middleware that sets a default User-Agent header on outbound
//! requests. Go parity: `applyUserAgentPassThrough`
//! (pass_through.go:171-213) — reads `pass_through_user_agent` from channel
//! settings (via pipeline context metadata). When pass-through is enabled,
//! the client's original User-Agent is preserved; otherwise `conduit/1.0`
//! is set.

use conduit_llm::HttpRequest;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Sets `User-Agent: conduit/1.0` on outbound requests unless the channel's
/// `pass_through_user_agent` setting is enabled, in which case the client's
/// User-Agent header is preserved.
pub struct DefaultUserAgentMiddleware;

impl PipelineMiddleware for DefaultUserAgentMiddleware {
    fn name(&self) -> &'static str {
        "default-user-agent"
    }

    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        mut request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        let pass_through = ctx
            .metadata
            .get("pass_through_user_agent")
            .map(|v| v == "true")
            .unwrap_or(false);

        if pass_through {
            // Pass-through enabled: preserve the client's User-Agent.
            // If none is present, set the default as a fallback.
            if !request.headers.contains_key("User-Agent") {
                request
                    .headers
                    .insert("User-Agent".to_string(), "conduit/1.0".to_string());
            }
        } else {
            // Pass-through disabled: always use Conduit API's default.
            request
                .headers
                .insert("User-Agent".to_string(), "conduit/1.0".to_string());
        }
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_default_user_agent() -> Result<(), Box<dyn std::error::Error>> {
        let mw = DefaultUserAgentMiddleware;
        let mut ctx = PipelineContext::new();
        let req = HttpRequest::default();
        let result = mw.on_outbound_raw_request(&mut ctx, req)?;
        assert_eq!(
            result.headers.get("User-Agent").map(|s| s.as_str()),
            Some("conduit/1.0")
        );
        Ok(())
    }

    #[test]
    fn overrides_custom_user_agent_when_pass_through_disabled()
    -> Result<(), Box<dyn std::error::Error>> {
        let mw = DefaultUserAgentMiddleware;
        let mut ctx = PipelineContext::new();
        let mut req = HttpRequest::default();
        req.headers
            .insert("User-Agent".to_string(), "custom/2.0".to_string());
        let result = mw.on_outbound_raw_request(&mut ctx, req)?;
        assert_eq!(
            result.headers.get("User-Agent").map(|s| s.as_str()),
            Some("conduit/1.0")
        );
        Ok(())
    }

    #[test]
    fn preserves_client_user_agent_when_pass_through_enabled()
    -> Result<(), Box<dyn std::error::Error>> {
        let mw = DefaultUserAgentMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("pass_through_user_agent".to_string(), "true".to_string());
        let mut req = HttpRequest::default();
        req.headers
            .insert("User-Agent".to_string(), "custom/2.0".to_string());
        let result = mw.on_outbound_raw_request(&mut ctx, req)?;
        assert_eq!(
            result.headers.get("User-Agent").map(|s| s.as_str()),
            Some("custom/2.0")
        );
        Ok(())
    }

    #[test]
    fn pass_through_with_no_ua_falls_back_to_default() -> Result<(), Box<dyn std::error::Error>> {
        let mw = DefaultUserAgentMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("pass_through_user_agent".to_string(), "true".to_string());
        let req = HttpRequest::default();
        let result = mw.on_outbound_raw_request(&mut ctx, req)?;
        assert_eq!(
            result.headers.get("User-Agent").map(|s| s.as_str()),
            Some("conduit/1.0")
        );
        Ok(())
    }
}
