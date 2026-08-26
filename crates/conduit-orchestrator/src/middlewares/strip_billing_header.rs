//! Pipeline middleware wrapping the `strip_billing_header_cch` transform (P-40).
//!
//! Go parity: `cc.StripBillingHeaderCCH()` (`orchestrator/orchestrator.go:77`).
//! Claude Code sends a billing marker inside the first system message; it must
//! be stripped before the request goes upstream so it is not forwarded to the
//! provider. The core transform is the pure
//! `crate::pre_execution::strip_billing_header_cch` function (unit-tested
//! there); this is the thin middleware that runs it on the inbound hook,
//! matching Go's global middleware position.

use conduit_llm::LlmRequest;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Strips the Claude Code billing marker (CCH) from the system message.
pub struct StripBillingHeaderMiddleware;

impl PipelineMiddleware for StripBillingHeaderMiddleware {
    fn name(&self) -> &'static str {
        "strip-billing-header-cch"
    }

    fn on_inbound_llm_request(
        &self,
        _ctx: &mut PipelineContext,
        mut request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        let _ = crate::pre_execution::strip_billing_header_cch(&mut request);
        Ok(request)
    }
}
