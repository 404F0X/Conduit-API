//! Standalone, deterministic NEW API + OpenAI-compatible upstream used for
//! local Conduit API end-to-end testing. It intentionally exposes dashboard/PAT,
//! quota, pricing, model discovery, JSON, SSE and representative multimodal
//! endpoints from one process.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, Response, StatusCode, header};
use axum::routing::any;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

pub const MOCK_PAT: &str = "mock-pat";
pub const MOCK_USER_ID: i64 = 19_301;
pub const MOCK_KEYS: &[(&str, &str)] = &[
    ("sk-mock-level1", "level1"),
    ("sk-mock-level2", "level2"),
    ("sk-mock-auto", "auto"),
];

const MODELS: &[&str] = &[
    "mock-chat",
    "mock-reasoning",
    "mock-vision",
    "mock-tool",
    "mock-embedding",
    "mock-rerank",
    "mock-image",
    "mock-audio",
    "mock-video",
    "mock-error-429",
    "mock-error-500",
];

#[derive(Debug, Clone)]
pub struct MockGatewayConfig {
    pub addr: SocketAddr,
}

impl Default for MockGatewayConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18_080),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockRequestRecord {
    pub method: String,
    pub path: String,
    pub model: Option<String>,
    pub stream: bool,
    pub body_bytes: usize,
    pub credential_kind: String,
    pub status: u16,
    pub recorded_at_unix_ms: u128,
}

#[derive(Clone, Default)]
struct GatewayState {
    requests: Arc<Mutex<Vec<MockRequestRecord>>>,
}

pub struct MockGatewayServer {
    addr: SocketAddr,
    state: GatewayState,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: JoinHandle<Result<(), std::io::Error>>,
}

impl MockGatewayServer {
    pub async fn start(config: MockGatewayConfig) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(config.addr).await?;
        let addr = listener.local_addr()?;
        let state = GatewayState::default();
        let app = Router::new()
            .fallback(any(handle_request))
            .with_state(state.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let join_handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
        });
        Ok(Self {
            addr,
            state,
            shutdown_tx: Some(shutdown_tx),
            join_handle,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub async fn recorded_requests(&self) -> Vec<MockRequestRecord> {
        self.state.requests.lock().await.clone()
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        match self.join_handle.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error.to_string()),
            Err(error) => Err(error.to_string()),
        }
    }
}

async fn handle_request(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Response<Body> {
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let path_and_query = request
        .uri()
        .path_and_query()
        .map(ToString::to_string)
        .unwrap_or_else(|| path.clone());
    let credential = bearer(request.headers().get(header::AUTHORIZATION));
    let credential_kind = match credential.as_deref() {
        Some(MOCK_PAT) => "pat",
        Some(value) if mock_key_group(value).is_some() => "model_key",
        Some(_) => "invalid",
        None => "none",
    }
    .to_string();
    let delay_ms = request
        .headers()
        .get("x-mock-delay-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(30_000);
    let body = to_bytes(request.into_body(), 32 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let payload = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    let model = payload["model"].as_str().map(ToOwned::to_owned);
    let stream = payload["stream"].as_bool() == Some(true);

    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }

    let response = dispatch(
        &method,
        &path,
        &path_and_query,
        credential.as_deref(),
        &payload,
        &state,
    )
    .await;
    let status = response.status().as_u16();
    if path != "/__mock/requests" {
        state.requests.lock().await.push(MockRequestRecord {
            method,
            path,
            model,
            stream,
            body_bytes: body.len(),
            credential_kind,
            status,
            recorded_at_unix_ms: now_millis(),
        });
    }
    response
}

async fn dispatch(
    method: &str,
    path: &str,
    path_and_query: &str,
    credential: Option<&str>,
    payload: &Value,
    state: &GatewayState,
) -> Response<Body> {
    match (method, path) {
        ("GET", "/") | ("GET", "/health") => json_response(
            StatusCode::OK,
            json!({"status":"ok","service":"conduit-new-api-mock"}),
        ),
        ("GET", "/__mock/config") => json_response(
            StatusCode::OK,
            json!({
                "pat": MOCK_PAT,
                "userId": MOCK_USER_ID,
                "modelKeys": MOCK_KEYS.iter().map(|(key, group)| json!({"key":key,"group":group})).collect::<Vec<_>>(),
                "models": MODELS,
                "failureModels": {"mock-error-429":429,"mock-error-500":500},
                "delayHeader":"x-mock-delay-ms"
            }),
        ),
        ("GET", "/__mock/requests") => json_response(
            StatusCode::OK,
            json!({"data":state.requests.lock().await.clone()}),
        ),
        ("DELETE", "/__mock/requests") => {
            state.requests.lock().await.clear();
            json_response(StatusCode::OK, json!({"success":true}))
        }
        ("GET", "/api/status") => json_response(
            StatusCode::OK,
            json!({
                "success":true,"data":{"quota_per_unit":500000,"quota_display_type":"USD","usd_exchange_rate":1}
            }),
        ),
        ("GET", "/api/ratio_config") => json_response(
            StatusCode::OK,
            json!({
                "success":true,"data":{"model_ratio":{"mock-chat":1,"mock-reasoning":2},"completion_ratio":{"mock-chat":3,"mock-reasoning":4},"cache_ratio":{"mock-chat":0.25}}
            }),
        ),
        ("GET", "/api/user/self") => pat_guard(credential, || {
            json_response(
                StatusCode::OK,
                json!({
                    "success":true,"data":{"id":MOCK_USER_ID,"username":"mock-buyer","group":"level3","quota":45000000,"used_quota":5000000}
                }),
            )
        }),
        ("GET", "/api/token/") => pat_guard(credential, || {
            json_response(
                StatusCode::OK,
                json!({
                    "success":true,"data":{"page":1,"page_size":100,"total":3,"items":[
                        token_row(1,"mock-level1","level1",json!(null)),
                        token_row(2,"mock-level2","level2",json!(null)),
                        token_row(3,"mock-auto","auto",json!(["level1","level2"]))
                    ]}
                }),
            )
        }),
        ("GET", "/api/pricing") => pat_guard(credential, || pricing_response()),
        ("GET", "/api/usage/token/") => model_key_guard(credential, || {
            let unlimited = credential == Some("sk-mock-auto");
            json_response(
                StatusCode::OK,
                json!({"code":true,"message":"ok","data":{
                    "object":"token_usage","name":"Conduit API mock key","total_granted":25000000,
                    "total_used":2500000,"total_available":22500000,"unlimited_quota":unlimited,"expires_at":0
                }}),
            )
        }),
        ("GET", "/v1/models") => model_key_guard(credential, || {
            json_response(
                StatusCode::OK,
                json!({
                    "object":"list","data":MODELS.iter().map(|id| json!({"id":id,"object":"model","created":1700000000,"owned_by":"conduit-mock"})).collect::<Vec<_>>()
                }),
            )
        }),
        ("GET", _) if path.starts_with("/v1/models/") => model_key_guard(credential, || {
            let id = path.trim_start_matches("/v1/models/");
            if MODELS.contains(&id) {
                json_response(
                    StatusCode::OK,
                    json!({"id":id,"object":"model","created":1700000000,"owned_by":"conduit-mock"}),
                )
            } else {
                openai_error(StatusCode::NOT_FOUND, "model_not_found", "model not found")
            }
        }),
        ("POST", "/v1/chat/completions") => model_key_guard(credential, || chat_response(payload)),
        ("POST", "/v1/completions") => model_key_guard(credential, || legacy_completion(payload)),
        ("POST", "/v1/responses") | ("POST", "/v1/responses/compact") => {
            model_key_guard(credential, || responses_response(payload))
        }
        ("POST", "/v1/embeddings") => model_key_guard(credential, || embeddings_response(payload)),
        ("POST", "/v1/rerank") | ("POST", "/jina/v1/rerank") => {
            model_key_guard(credential, || rerank_response(payload))
        }
        ("POST", "/v1/images/generations") | ("POST", "/v1/images/edits") => {
            model_key_guard(credential, image_response)
        }
        ("POST", "/v1/audio/transcriptions") | ("POST", "/v1/audio/translations") => {
            model_key_guard(credential, || {
                json_response(StatusCode::OK, json!({"text":"mock audio transcript"}))
            })
        }
        ("POST", "/v1/audio/speech") => model_key_guard(credential, audio_response),
        ("POST", "/v1/moderations") => model_key_guard(credential, || {
            json_response(
                StatusCode::OK,
                json!({"id":request_id("modr"),"model":"mock-moderation","results":[{"flagged":false,"categories":{},"category_scores":{}}]}),
            )
        }),
        ("POST", "/v1/videos") => model_key_guard(credential, || {
            json_response(
                StatusCode::OK,
                json!({"id":"video_mock_1","object":"video","status":"queued","progress":0}),
            )
        }),
        ("GET", "/v1/videos/video_mock_1") => model_key_guard(credential, || {
            json_response(
                StatusCode::OK,
                json!({"id":"video_mock_1","object":"video","status":"completed","progress":100,"url":"data:video/mp4;base64,AAAA"}),
            )
        }),
        _ if path_and_query.starts_with("/api/token/?") => pat_guard(credential, || {
            json_response(
                StatusCode::OK,
                json!({
                    "success":true,"data":{"page":1,"page_size":100,"total":3,"items":[
                        token_row(1,"mock-level1","level1",json!(null)), token_row(2,"mock-level2","level2",json!(null)), token_row(3,"mock-auto","auto",json!(["level1","level2"]))
                    ]}
                }),
            )
        }),
        _ => openai_error(
            StatusCode::NOT_FOUND,
            "route_not_found",
            "mock route not found",
        ),
    }
}

fn pricing_response() -> Response<Body> {
    let token_price = |name: &str, ratio: f64, completion: f64, cache: Option<f64>| {
        json!({
            "model_name":name,"quota_type":0,"model_ratio":ratio,"model_price":0,
            "completion_ratio":completion,"cache_ratio":cache,"create_cache_ratio":0,
            "enable_groups":["all"],"supported_endpoint_types":["openai"]
        })
    };
    let request_price = |name: &str, price: f64| {
        json!({
            "model_name":name,"quota_type":1,"model_ratio":0,"model_price":price,
            "completion_ratio":1,"enable_groups":["all"],"supported_endpoint_types":["openai"]
        })
    };
    json_response(
        StatusCode::OK,
        json!({
            "success":true,
            "data":[
                token_price("mock-chat",1.0,3.0,Some(0.25)),
                token_price("mock-reasoning",2.0,4.0,Some(0.5)),
                token_price("mock-vision",1.5,3.0,None),
                token_price("mock-tool",1.25,3.0,None),
                token_price("mock-embedding",0.05,1.0,None),
                request_price("mock-rerank",0.002),request_price("mock-image",0.01),
                request_price("mock-audio",0.005),request_price("mock-video",0.05),
                token_price("mock-error-429",1.0,3.0,None),token_price("mock-error-500",1.0,3.0,None)
            ],
            "vendors":[{"id":1,"name":"Conduit API Mock"}],
            "group_ratio":{"level1":1.0,"level2":1.5,"level3":2.0},
            "usable_group":{"level1":"Level 1","level2":"Level 2","level3":"Level 3"},
            "auto_groups":["level1","level2"],"pricing_version":"mock-pricing-v1"
        }),
    )
}

fn token_row(id: i64, raw_key: &str, group: &str, auto_groups: Value) -> Value {
    json!({"id":id,"user_id":MOCK_USER_ID,"key":mask_token(raw_key),"status":1,"name":format!("Mock {group}"),
        "created_time":1700000000,"accessed_time":0,"expired_time":-1,"remain_quota":25000000,
        "unlimited_quota":group=="auto","model_limits_enabled":false,"model_limits":"","allow_ips":"",
        "used_quota":2500000,"group":group,"auto_groups":auto_groups,"cross_group_retry":group=="auto"})
}

fn chat_response(payload: &Value) -> Response<Body> {
    let model = payload["model"].as_str().unwrap_or("mock-chat");
    if model == "mock-error-429" {
        return openai_error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_exceeded",
            "simulated rate limit",
        );
    }
    if model == "mock-error-500" {
        return openai_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upstream_error",
            "simulated upstream failure",
        );
    }
    let id = request_id("chatcmpl");
    let tool = model == "mock-tool" || payload["tools"].as_array().is_some_and(|v| !v.is_empty());
    if payload["stream"].as_bool() == Some(true) {
        let first_delta = if tool {
            json!({"tool_calls":[{"index":0,"id":"call_mock_1","type":"function","function":{"name":"mock_weather","arguments":"{\"city\":\""}}]})
        } else {
            json!({"role":"assistant","content":"Conduit API mock "})
        };
        let second_delta = if tool {
            json!({"tool_calls":[{"index":0,"function":{"arguments":"Shanghai\"}"}}]})
        } else {
            json!({"content":"stream works."})
        };
        let finish = if tool { "tool_calls" } else { "stop" };
        let frames = [
            json!({"id":id,"object":"chat.completion.chunk","created":1700000000,"model":model,"choices":[{"index":0,"delta":first_delta,"finish_reason":null}]}),
            json!({"id":id,"object":"chat.completion.chunk","created":1700000000,"model":model,"choices":[{"index":0,"delta":second_delta,"finish_reason":null}]}),
            json!({"id":id,"object":"chat.completion.chunk","created":1700000000,"model":model,"choices":[{"index":0,"delta":{},"finish_reason":finish}],"usage":usage(model)}),
        ];
        return sse_response(
            frames
                .into_iter()
                .map(|frame| format!("data: {frame}\n\n"))
                .collect::<String>()
                + "data: [DONE]\n\n",
        );
    }
    let message = if tool {
        json!({"role":"assistant","content":null,"tool_calls":[{"id":"call_mock_1","type":"function","function":{"name":"mock_weather","arguments":"{\"city\":\"Shanghai\"}"}}]})
    } else {
        json!({"role":"assistant","content":"Conduit API Rust reached the simulated NEW API gateway successfully."})
    };
    json_response(
        StatusCode::OK,
        json!({"id":id,"object":"chat.completion","created":1700000000,"model":model,
        "choices":[{"index":0,"message":message,"finish_reason":if tool {"tool_calls"} else {"stop"},"logprobs":null}],"usage":usage(model),
        "system_fingerprint":"fp_conduit_mock"}),
    )
}

fn responses_response(payload: &Value) -> Response<Body> {
    let model = payload["model"].as_str().unwrap_or("mock-reasoning");
    let id = request_id("resp");
    let completed = json!({"id":id,"object":"response","created_at":1700000000,"status":"completed","model":model,
        "output":[{"id":"msg_mock_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Mock Responses API works.","annotations":[]}]}],
        "usage":{"input_tokens":120,"input_tokens_details":{"cached_tokens":20},"output_tokens":40,"output_tokens_details":{"reasoning_tokens":12},"total_tokens":160}});
    if payload["stream"].as_bool() == Some(true) {
        let events = [
            (
                "response.created",
                json!({"type":"response.created","response":{"id":id,"object":"response","status":"in_progress","model":model}}),
            ),
            (
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","item_id":"msg_mock_1","output_index":0,"content_index":0,"delta":"Mock Responses "}),
            ),
            (
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","item_id":"msg_mock_1","output_index":0,"content_index":0,"delta":"API works."}),
            ),
            (
                "response.completed",
                json!({"type":"response.completed","response":completed}),
            ),
        ];
        return sse_response(
            events
                .into_iter()
                .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
                .collect(),
        );
    }
    json_response(StatusCode::OK, completed)
}

fn legacy_completion(payload: &Value) -> Response<Body> {
    let model = payload["model"].as_str().unwrap_or("mock-chat");
    json_response(
        StatusCode::OK,
        json!({"id":request_id("cmpl"),"object":"text_completion","created":1700000000,"model":model,
        "choices":[{"index":0,"text":"mock completion","finish_reason":"stop","logprobs":null}],"usage":usage(model)}),
    )
}

fn embeddings_response(payload: &Value) -> Response<Body> {
    let count = payload["input"].as_array().map_or(1, Vec::len);
    let data = (0..count)
        .map(|index| json!({"object":"embedding","index":index,"embedding":[0.01,0.02,0.03,0.04]}))
        .collect::<Vec<_>>();
    json_response(
        StatusCode::OK,
        json!({"object":"list","data":data,"model":payload["model"],"usage":{"prompt_tokens":8,"total_tokens":8}}),
    )
}

fn rerank_response(payload: &Value) -> Response<Body> {
    let count = payload["documents"].as_array().map_or(1, Vec::len).min(3);
    let results = (0..count)
        .map(|index| json!({"index":index,"relevance_score":1.0-(index as f64*0.2)}))
        .collect::<Vec<_>>();
    json_response(
        StatusCode::OK,
        json!({"id":request_id("rerank"),"results":results,"usage":{"total_tokens":16}}),
    )
}

fn image_response() -> Response<Body> {
    json_response(
        StatusCode::OK,
        json!({"created":1700000000,"data":[{"b64_json":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=","revised_prompt":"Conduit API mock image"}]}),
    )
}

fn audio_response() -> Response<Body> {
    response(StatusCode::OK, "audio/wav", b"RIFFmockWAVEfmt ".to_vec())
}

fn usage(model: &str) -> Value {
    // Keep the ordinary chat fixture small while giving billing E2E tests a
    // deterministic high-volume request. This lets the full procurement ->
    // retail -> subscription settlement path be exercised without sending a
    // huge prompt over the local socket.
    let (prompt_tokens, completion_tokens, cached_tokens, reasoning_tokens) =
        if model == "mock-reasoning" {
            (120_000, 40_000, 20_000, 12_000)
        } else {
            (120, 40, 20, 0)
        };
    json!({"prompt_tokens":prompt_tokens,"completion_tokens":completion_tokens,"total_tokens":prompt_tokens+completion_tokens,
        "prompt_tokens_details":{"cached_tokens":cached_tokens,"audio_tokens":0},
        "completion_tokens_details":{"reasoning_tokens":reasoning_tokens,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0}})
}

fn bearer(value: Option<&axum::http::HeaderValue>) -> Option<String> {
    value?
        .to_str()
        .ok()?
        .trim()
        .strip_prefix("Bearer ")
        .map(ToOwned::to_owned)
}

fn mock_key_group(key: &str) -> Option<&'static str> {
    MOCK_KEYS
        .iter()
        .find_map(|(candidate, group)| (*candidate == key).then_some(*group))
}

fn pat_guard(credential: Option<&str>, success: impl FnOnce() -> Response<Body>) -> Response<Body> {
    if credential == Some(MOCK_PAT) {
        success()
    } else {
        openai_error(StatusCode::UNAUTHORIZED, "invalid_pat", "invalid mock PAT")
    }
}

fn model_key_guard(
    credential: Option<&str>,
    success: impl FnOnce() -> Response<Body>,
) -> Response<Body> {
    if credential.and_then(mock_key_group).is_some() {
        success()
    } else {
        openai_error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "invalid mock model key",
        )
    }
}

fn mask_token(key: &str) -> String {
    let chars = key.chars().collect::<Vec<_>>();
    if chars.len() <= 4 {
        return "*".repeat(chars.len());
    }
    if chars.len() <= 8 {
        return format!(
            "{}****{}",
            chars[..2].iter().collect::<String>(),
            chars[chars.len() - 2..].iter().collect::<String>()
        );
    }
    format!(
        "{}**********{}",
        chars[..4].iter().collect::<String>(),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
}

fn openai_error(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    json_response(
        status,
        json!({"error":{"message":message,"type":code,"param":null,"code":code}}),
    )
}

fn json_response(status: StatusCode, value: Value) -> Response<Body> {
    response(status, "application/json", value.to_string().into_bytes())
}

fn sse_response(value: String) -> Response<Body> {
    response(StatusCode::OK, "text/event-stream", value.into_bytes())
}

fn response(status: StatusCode, content_type: &str, bytes: Vec<u8>) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header("x-mock-gateway", "conduit-new-api")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn request_id(prefix: &str) -> String {
    format!("{prefix}_mock_{}", now_millis())
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Client;

    async fn start_gateway() -> Result<MockGatewayServer, std::io::Error> {
        MockGatewayServer::start(MockGatewayConfig {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        })
        .await
    }

    #[tokio::test]
    async fn pat_can_read_pricing() -> Result<(), Box<dyn std::error::Error>> {
        let server = start_gateway().await?;
        let response = Client::new()
            .get(format!("{}/api/pricing", server.base_url()))
            .bearer_auth(MOCK_PAT)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await?;
        assert_eq!(body["success"], true);
        assert_eq!(body["group_ratio"]["level2"], 1.5);
        assert!(
            body["data"]
                .as_array()
                .ok_or("pricing data should be an array")?
                .iter()
                .any(|row| row["model_name"] == "mock-chat")
        );
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn pat_token_list_exposes_groups_but_masks_keys() -> Result<(), Box<dyn std::error::Error>>
    {
        let server = start_gateway().await?;
        let body: Value = Client::new()
            .get(format!("{}/api/token/?p=0&size=100", server.base_url()))
            .bearer_auth(MOCK_PAT)
            .send()
            .await?
            .json()
            .await?;
        let items = body["data"]["items"]
            .as_array()
            .ok_or("token items should be an array")?;
        assert_eq!(items.len(), MOCK_KEYS.len());
        assert_eq!(items[0]["group"], "level1");
        assert_eq!(items[2]["auto_groups"], json!(["level1", "level2"]));
        for item in items {
            let key = item["key"]
                .as_str()
                .ok_or("masked key should be a string")?;
            assert!(key.contains('*'));
            assert!(!MOCK_KEYS.iter().any(|(raw, _)| *raw == key));
        }
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn chat_supports_json_and_sse() -> Result<(), Box<dyn std::error::Error>> {
        let server = start_gateway().await?;
        let client = Client::new();
        let json_response = client
            .post(format!("{}/v1/chat/completions", server.base_url()))
            .bearer_auth("sk-mock-level1")
            .json(&json!({"model":"mock-chat","messages":[{"role":"user","content":"ping"}]}))
            .send()
            .await?;
        assert_eq!(json_response.status(), StatusCode::OK);
        let json_body: Value = json_response.json().await?;
        assert_eq!(json_body["object"], "chat.completion");
        assert_eq!(json_body["usage"]["total_tokens"], 160);

        let sse_response = client
            .post(format!("{}/v1/chat/completions", server.base_url()))
            .bearer_auth("sk-mock-level1")
            .json(&json!({"model":"mock-chat","messages":[],"stream":true}))
            .send()
            .await?;
        assert_eq!(sse_response.status(), StatusCode::OK);
        assert_eq!(
            sse_response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        let sse_body = sse_response.text().await?;
        assert!(sse_body.contains("chat.completion.chunk"));
        assert!(sse_body.ends_with("data: [DONE]\n\n"));
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn failure_model_returns_429_and_request_is_logged()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = start_gateway().await?;
        let response = Client::new()
            .post(format!("{}/v1/chat/completions", server.base_url()))
            .bearer_auth("sk-mock-level2")
            .json(&json!({"model":"mock-error-429","messages":[]}))
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        let records = server.recorded_requests().await;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, "/v1/chat/completions");
        assert_eq!(records[0].model.as_deref(), Some("mock-error-429"));
        assert_eq!(records[0].credential_kind, "model_key");
        assert_eq!(records[0].status, 429);
        server.shutdown().await?;
        Ok(())
    }
}
