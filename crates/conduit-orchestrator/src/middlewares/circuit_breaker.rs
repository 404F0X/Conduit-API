//! Pipeline middleware that tracks per-model error counts and enters a cooldown
//! state when too many consecutive errors occur, rejecting requests to that
//! model during cooldown. Simplified port of Go `modelCircuitBreakerTracker`
//! (`conduit/internal/server/orchestrator/model_circuit_breaker.go`).
//!
//! Go source uses `biz.ModelCircuitBreaker` with Open/HalfOpen/Closed states
//! and probe logic. This Rust port implements the same core pattern:
//! - `Closed` (normal): requests pass through.
//! - `Open` (cooldown): requests are rejected until cooldown expires.
//! - `HalfOpen`: exactly one provider probe is admitted.
//! - A successful response resets the error count immediately.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use conduit_core::{ConduitError, ErrorKind};
use conduit_llm::{HttpRequest, HttpResponse};
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Per-model circuit state: tracks consecutive errors and optional cooldown.
#[derive(Debug, Clone)]
struct CircuitState {
    error_count: u32,
    cooldown_until: Option<Instant>,
    half_open_probe_in_flight: bool,
}

/// Model circuit breaker middleware configuration.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive errors before entering cooldown (Go: `OpenThreshold`).
    pub max_errors: u32,
    /// Duration of cooldown period once the threshold is breached.
    pub cooldown_duration: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_errors: 5,
            cooldown_duration: Duration::from_secs(60),
        }
    }
}

/// Shared circuit-breaker state map, keyed by model ID.
type SharedState = Arc<Mutex<HashMap<String, CircuitState>>>;

/// Pipeline middleware that rejects requests to models whose error count has
/// breached the configured threshold, holding them in cooldown for a fixed
/// duration. Successful responses reset the circuit immediately.
///
/// Go parity: `modelCircuitBreakerTracker`
/// (`orchestrator/model_circuit_breaker.go:23-33`).
pub struct ModelCircuitBreakerMiddleware {
    state: SharedState,
    config: CircuitBreakerConfig,
}

impl ModelCircuitBreakerMiddleware {
    /// Create a new circuit breaker with the given config.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    /// Create a new circuit breaker with default config (max_errors=5,
    /// cooldown=60s).
    pub fn with_defaults() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }

    /// Resolve the model identifier from the request and context. Prefers
    /// `actual_model` from metadata (set by the outbound transformer after
    /// model mapping), falling back to `request.model`.
    fn resolve_model(ctx: &PipelineContext, request_model: Option<&str>) -> Option<String> {
        ctx.metadata
            .get("actual_model")
            .cloned()
            .or_else(|| request_model.map(|s| s.to_string()))
    }

    fn resolve_key(ctx: &PipelineContext, request_model: Option<&str>) -> Option<String> {
        let model = Self::resolve_model(ctx, request_model)?;
        let channel = ctx
            .metadata
            .get("channel_id")
            .map(String::as_str)
            .unwrap_or("");
        let credential = ctx
            .metadata
            .get("credential_identity")
            .map(String::as_str)
            .unwrap_or("");
        if channel.is_empty() && credential.is_empty() {
            Some(model)
        } else {
            Some(format!("{channel}\u{1f}{model}\u{1f}{credential}"))
        }
    }

    /// Go only installs the model-circuit-breaker tracker for the effective
    /// `circuit-breaker` load-balancing strategy. The orchestrator stamps the
    /// resolved (system default + API-key override) value per request. Missing
    /// metadata keeps the historical standalone-middleware behavior for tests
    /// and non-orchestrator callers.
    fn enabled_for(ctx: &PipelineContext) -> bool {
        ctx.metadata
            .get(crate::orchestrator::LOAD_BALANCE_STRATEGY_METADATA)
            .is_none_or(|strategy| strategy == "circuit-breaker")
    }
}

impl PipelineMiddleware for ModelCircuitBreakerMiddleware {
    fn name(&self) -> &'static str {
        "model-circuit-breaker"
    }

    /// Check if the model is currently in cooldown; if so, reject.
    ///
    /// This runs on `on_outbound_raw_request` — NOT `on_inbound_llm_request` —
    /// to match Go `OnOutboundRawRequest` (model_circuit_breaker.go:39-68) and,
    /// critically, to fix a key mismatch (P-36): `actual_model` is stamped onto
    /// the context inside the retry loop (pipeline stamps it per attempt), which
    /// happens AFTER the inbound hook but BEFORE the outbound-request hook. The
    /// old inbound check therefore keyed on `request.model` (the client's
    /// requested name) while the error-counting hooks keyed on `actual_model`
    /// (the channel's upstream name) — two different HashMap rows, so the
    /// breaker never opened. Checking here makes both use the same key.
    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        if !Self::enabled_for(ctx) {
            return Ok(request);
        }
        let key = match Self::resolve_key(ctx, None) {
            Some(m) if !m.is_empty() => m,
            _ => return Ok(request),
        };
        let model = Self::resolve_model(ctx, None).unwrap_or_default();

        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(circuit) = guard.get_mut(&key) {
            if let Some(until) = circuit.cooldown_until {
                if Instant::now() < until {
                    return Err(ConduitError::new(
                        ErrorKind::Upstream,
                        format!(
                            "model circuit breaker open: {model} is in cooldown after {} consecutive errors",
                            circuit.error_count,
                        ),
                    )
                    .with_code("model_circuit_open"));
                }
                // Cooldown elapsed: allow exactly one probe. Concurrent
                // requests remain rejected until that probe succeeds/fails.
                circuit.cooldown_until = None;
                circuit.half_open_probe_in_flight = true;
            } else if circuit.half_open_probe_in_flight {
                return Err(ConduitError::new(
                    ErrorKind::Upstream,
                    format!("model circuit breaker half-open: probe in flight for {model}"),
                )
                .with_code("model_circuit_half_open"));
            }
        }

        Ok(request)
    }

    /// On a successful raw response, reset the error count for the model.
    /// Go parity: `OnOutboundLlmResponse` (model_circuit_breaker.go:71-83)
    /// calls `RecordSuccess` which resets `State→Closed` and clears failures.
    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        if !Self::enabled_for(ctx) {
            return Ok(response);
        }
        let key =
            match Self::resolve_key(ctx, ctx.metadata.get("request_model").map(|s| s.as_str())) {
                Some(m) if !m.is_empty() => m,
                _ => return Ok(response),
            };

        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(circuit) = guard.get_mut(&key) {
            circuit.error_count = 0;
            circuit.cooldown_until = None;
            circuit.half_open_probe_in_flight = false;
        }

        Ok(response)
    }

    /// On an outbound error, increment the error count for the model. If the
    /// count reaches `max_errors`, enter cooldown. Go parity:
    /// `OnOutboundRawError` (model_circuit_breaker.go:85-107) calls
    /// `RecordError` which increments `ConsecutiveFailures` and may transition
    /// to `StateOpen`.
    fn on_outbound_raw_error(&self, ctx: &mut PipelineContext, error: &ConduitError) {
        if !Self::enabled_for(ctx) {
            return;
        }
        if error
            .code
            .as_deref()
            .is_some_and(|code| matches!(code, "model_circuit_open" | "model_circuit_half_open"))
        {
            return;
        }
        let key =
            match Self::resolve_key(ctx, ctx.metadata.get("request_model").map(|s| s.as_str())) {
                Some(m) if !m.is_empty() => m,
                _ => return,
            };
        // Client mistakes must not take a healthy provider out of service.
        // Count transport failures, 429, and server-side provider failures.
        if error.kind != ErrorKind::Upstream {
            // A later local middleware rejected the half-open request before
            // it reached the provider. Release the single probe slot without
            // treating that local admission result as provider health.
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(circuit) = guard.get_mut(&key)
                && circuit.half_open_probe_in_flight
            {
                circuit.half_open_probe_in_flight = false;
                circuit.cooldown_until = Some(Instant::now());
            }
            return;
        }
        if error
            .provider_status
            .is_some_and(|status| (400..500).contains(&status) && status != 429)
        {
            // A provider 4xx proves the route is reachable. It is a successful
            // half-open health probe even though the caller's request failed.
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(circuit) = guard.get_mut(&key) {
                circuit.error_count = 0;
                circuit.cooldown_until = None;
                circuit.half_open_probe_in_flight = false;
            }
            return;
        }

        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let circuit = guard.entry(key).or_insert(CircuitState {
            error_count: 0,
            cooldown_until: None,
            half_open_probe_in_flight: false,
        });

        circuit.error_count = circuit.error_count.saturating_add(1);
        if circuit.half_open_probe_in_flight || circuit.error_count >= self.config.max_errors {
            circuit.cooldown_until = Some(Instant::now() + self.config.cooldown_duration);
            circuit.half_open_probe_in_flight = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the ctx the way the pipeline does inside the retry loop: the
    /// per-attempt `actual_model` is stamped before the outbound-request hook
    /// runs. The circuit breaker checks on `on_outbound_raw_request`, so this is
    /// the state it sees.
    fn ctx_with_actual_model(model: &str) -> PipelineContext {
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("actual_model".to_string(), model.to_string());
        ctx
    }

    fn http_request() -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            ..HttpRequest::default()
        }
    }

    #[test]
    fn normal_request_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ModelCircuitBreakerMiddleware::with_defaults();
        let mut ctx = ctx_with_actual_model("gpt-4o");

        // Not in cooldown → passes.
        let result = mw.on_outbound_raw_request(&mut ctx, http_request())?;
        assert_eq!(result.path, "/v1/chat/completions");
        Ok(())
    }

    #[test]
    fn adaptive_strategy_does_not_track_or_reject_with_circuit_breaker()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = CircuitBreakerConfig {
            max_errors: 1,
            cooldown_duration: Duration::from_secs(300),
        };
        let mw = ModelCircuitBreakerMiddleware::new(config);
        let error = ConduitError::upstream("test error");
        let mut ctx = ctx_with_actual_model("gpt-4o");
        ctx.metadata.insert(
            crate::orchestrator::LOAD_BALANCE_STRATEGY_METADATA.to_string(),
            "adaptive".to_string(),
        );

        mw.on_outbound_raw_error(&mut ctx, &error);
        let request = mw.on_outbound_raw_request(&mut ctx, http_request())?;

        assert_eq!(request.path, "/v1/chat/completions");
        assert!(
            mw.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        );
        Ok(())
    }

    /// P-36 regression: the KEY the check reads must equal the KEY the error
    /// counter writes. Errors are counted under `actual_model`; the check must
    /// therefore also key on `actual_model` (it now runs on the same hook, so
    /// it does). Before the fix the check keyed on `request.model` and never saw
    /// the accumulated failures — the breaker never opened.
    #[test]
    fn check_and_count_use_the_same_key_so_breaker_opens() -> Result<(), Box<dyn std::error::Error>>
    {
        let config = CircuitBreakerConfig {
            max_errors: 2,
            cooldown_duration: Duration::from_secs(300),
        };
        let mw = ModelCircuitBreakerMiddleware::new(config);
        let error = ConduitError::upstream("test error");

        // Count 2 errors under the upstream (actual) model name.
        for _ in 0..2 {
            let mut ctx = ctx_with_actual_model("provider/gpt-4o-mapped");
            mw.on_outbound_raw_error(&mut ctx, &error);
        }

        // The very next outbound request for that same actual model must be
        // rejected — proving the check read the same HashMap row the counter
        // wrote.
        let mut ctx = ctx_with_actual_model("provider/gpt-4o-mapped");
        let result = mw.on_outbound_raw_request(&mut ctx, http_request());
        assert!(
            result.is_err(),
            "breaker must open for the model the errors were counted under (P-36)"
        );
        Ok(())
    }

    #[test]
    fn error_accumulation_triggers_cooldown() -> Result<(), Box<dyn std::error::Error>> {
        let config = CircuitBreakerConfig {
            max_errors: 3,
            cooldown_duration: Duration::from_secs(60),
        };
        let mw = ModelCircuitBreakerMiddleware::new(config);
        let error = ConduitError::upstream("test error");

        // Record 3 errors for the model (threshold = 3).
        for _ in 0..3 {
            let mut ctx = PipelineContext::new();
            ctx.metadata
                .insert("actual_model".to_string(), "gpt-4o".to_string());
            mw.on_outbound_raw_error(&mut ctx, &error);
        }

        // Verify the circuit is now open.
        let guard = mw.state.lock().unwrap_or_else(|e| e.into_inner());
        let circuit = guard.get("gpt-4o");
        assert!(circuit.is_some(), "circuit state must exist for gpt-4o");
        let circuit = match circuit {
            Some(c) => c,
            None => return Err("missing circuit state".into()),
        };
        assert_eq!(circuit.error_count, 3);
        assert!(
            circuit.cooldown_until.is_some(),
            "cooldown must be set after reaching threshold"
        );
        Ok(())
    }

    #[test]
    fn cooldown_rejects_request() -> Result<(), Box<dyn std::error::Error>> {
        let config = CircuitBreakerConfig {
            max_errors: 2,
            cooldown_duration: Duration::from_secs(300),
        };
        let mw = ModelCircuitBreakerMiddleware::new(config);
        let error = ConduitError::upstream("test error");

        // Drive the model into cooldown (2 errors with threshold = 2).
        for _ in 0..2 {
            let mut ctx = PipelineContext::new();
            ctx.metadata
                .insert("actual_model".to_string(), "claude-3".to_string());
            mw.on_outbound_raw_error(&mut ctx, &error);
        }

        // Inbound request for the same model should be rejected.
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("actual_model".to_string(), "claude-3".to_string());
        let result = mw.on_outbound_raw_request(&mut ctx, http_request());
        assert!(result.is_err(), "request must be rejected during cooldown");

        // A different model should still pass.
        let mut ctx2 = ctx_with_actual_model("gpt-4o");
        let result2 = mw.on_outbound_raw_request(&mut ctx2, http_request())?;
        assert_eq!(result2.path, "/v1/chat/completions");

        Ok(())
    }

    #[test]
    fn success_resets_error_count() -> Result<(), Box<dyn std::error::Error>> {
        let config = CircuitBreakerConfig {
            max_errors: 3,
            cooldown_duration: Duration::from_secs(60),
        };
        let mw = ModelCircuitBreakerMiddleware::new(config);
        let error = ConduitError::upstream("test error");

        // Record 2 errors (below threshold of 3).
        for _ in 0..2 {
            let mut ctx = PipelineContext::new();
            ctx.metadata
                .insert("actual_model".to_string(), "gpt-4o".to_string());
            mw.on_outbound_raw_error(&mut ctx, &error);
        }

        // Successful response resets the circuit.
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("request_model".to_string(), "gpt-4o".to_string());
        let response = HttpResponse::default();
        let _ = mw.on_outbound_raw_response(&mut ctx, response)?;

        // Verify error count is reset.
        let guard = mw.state.lock().unwrap_or_else(|e| e.into_inner());
        let circuit = guard.get("gpt-4o");
        assert!(circuit.is_some(), "circuit state must still exist");
        let circuit = match circuit {
            Some(c) => c,
            None => return Err("missing circuit state".into()),
        };
        assert_eq!(circuit.error_count, 0, "error count must be reset to 0");
        assert!(
            circuit.cooldown_until.is_none(),
            "cooldown must be cleared after success"
        );

        // After reset, 2 more errors should NOT trigger cooldown (still below 3).
        drop(guard);
        for _ in 0..2 {
            let mut ctx = PipelineContext::new();
            ctx.metadata
                .insert("actual_model".to_string(), "gpt-4o".to_string());
            mw.on_outbound_raw_error(&mut ctx, &error);
        }
        let mut ctx = ctx_with_actual_model("gpt-4o");
        let result = mw.on_outbound_raw_request(&mut ctx, http_request())?;
        assert_eq!(result.path, "/v1/chat/completions");

        Ok(())
    }

    #[test]
    fn client_400_does_not_open_the_circuit() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ModelCircuitBreakerMiddleware::new(CircuitBreakerConfig {
            max_errors: 1,
            cooldown_duration: Duration::from_secs(60),
        });
        let error = ConduitError::upstream("bad request").with_provider_status(400);
        let mut ctx = ctx_with_actual_model("gpt-4o");

        mw.on_outbound_raw_error(&mut ctx, &error);
        mw.on_outbound_raw_request(&mut ctx, http_request())?;
        assert!(
            mw.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn circuit_isolated_by_channel_and_credential() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ModelCircuitBreakerMiddleware::new(CircuitBreakerConfig {
            max_errors: 1,
            cooldown_duration: Duration::from_secs(60),
        });
        let error = ConduitError::upstream("provider failure").with_provider_status(503);
        let mut failed = ctx_with_actual_model("gpt-4o");
        failed.metadata.insert("channel_id".into(), "a".into());
        failed
            .metadata
            .insert("credential_identity".into(), "key-1".into());
        mw.on_outbound_raw_error(&mut failed, &error);

        let mut healthy = ctx_with_actual_model("gpt-4o");
        healthy.metadata.insert("channel_id".into(), "b".into());
        healthy
            .metadata
            .insert("credential_identity".into(), "key-2".into());
        mw.on_outbound_raw_request(&mut healthy, http_request())?;
        assert!(
            mw.on_outbound_raw_request(&mut failed, http_request())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn cooldown_transitions_to_single_half_open_probe() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ModelCircuitBreakerMiddleware::new(CircuitBreakerConfig {
            max_errors: 1,
            cooldown_duration: Duration::ZERO,
        });
        let error = ConduitError::upstream("provider failure").with_provider_status(503);
        let mut ctx = ctx_with_actual_model("gpt-4o");
        mw.on_outbound_raw_error(&mut ctx, &error);

        mw.on_outbound_raw_request(&mut ctx, http_request())?;
        assert!(
            mw.on_outbound_raw_request(&mut ctx, http_request())
                .is_err()
        );
        mw.on_outbound_raw_response(&mut ctx, HttpResponse::default())?;
        mw.on_outbound_raw_request(&mut ctx, http_request())?;
        Ok(())
    }

    #[test]
    fn local_rejection_releases_half_open_probe_slot() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ModelCircuitBreakerMiddleware::new(CircuitBreakerConfig {
            max_errors: 1,
            cooldown_duration: Duration::ZERO,
        });
        let provider_error = ConduitError::upstream("provider failure").with_provider_status(503);
        let mut ctx = ctx_with_actual_model("gpt-4o");
        mw.on_outbound_raw_error(&mut ctx, &provider_error);

        mw.on_outbound_raw_request(&mut ctx, http_request())?;
        mw.on_outbound_raw_error(&mut ctx, &ConduitError::rate_limited("local admission"));

        mw.on_outbound_raw_request(&mut ctx, http_request())?;
        Ok(())
    }
}
