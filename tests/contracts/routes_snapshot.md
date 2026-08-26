# Conduit API HTTP Route Snapshot

Hand-compiled from `conduit/internal/server/routes.go` (+ the OIDC
`RegisterRoutes` sub-registration in `conduit/internal/server/api/oidc.go`)
per `RUST-P0-001` S05. Every row below is a real route from the Go source —
**no entry is synthesized**. Pending independent verification (drift audit).

Source commit basis: the `SetupRoutes` function in `routes.go` and
`OIDCHandlers.RegisterRoutes` in `oidc.go`.

## Format

```
METHOD PATH | auth=<public|jwt|api_key|gemini_key|openapi_service_account> | timeout=<request|llm|none> | middleware=[group/route-specific] | handler=<Go handler>
```

- `auth` = the group's auth middleware (`public` = no auth middleware).
- `timeout` = `request` (`WithTimeout(Config.RequestTimeout)`), `llm`
  (`WithTimeout(Config.LLMRequestTimeout)`), or `none` (no `WithTimeout` on the
  route or its group).
- `middleware=[...]` lists the group/route-specific middleware only.
- **Global middleware applied to EVERY route** (via `engine.Use` / `server.Use`
  in `New` + `SetupRoutes`): `Recovery`, `AccessLog`, `WithEntClient`,
  `WithLoggingTracing`, `WithMetrics` — plus `CORS` + `OPTIONS *` when
  `Config.CORS.Enabled`.

## Static fallback

```
STATIC_FALLBACK * | auth=public | timeout=none | middleware=[static] | handler=static.Handler (server.NoRoute — SPA fallback; real 404 only when no SPA route matches)
```

## public — `server.Group("", WithTimeout(RequestTimeout))`  · auth=public · timeout=request

```
GET /favicon | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.System.GetFavicon
GET /health | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.System.Health
```

## unSecureAdmin — `server.Group("/admin", WithTimeout(RequestTimeout))`  · auth=public · timeout=request

```
GET /admin/system/status | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.System.GetSystemStatus
POST /admin/system/initialize | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.System.InitializeSystem
POST /admin/auth/signin | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.Auth.SignIn
```

## oauth — `server.Group("/oauth", WithTimeout(RequestTimeout))`  · auth=public · timeout=request
OIDC routes registered via `handlers.OIDC.RegisterRoutes(oauthGroup)` (sub-group `/oidc`).

```
GET /oauth/oidc/providers | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.OIDC.GetProviders
GET /oauth/oidc/authorize/:provider | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.OIDC.GetAuthorizeURL
GET /oauth/oidc/callback | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.OIDC.Callback
GET /oauth/oidc/callback/:provider | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.OIDC.Callback
POST /oauth/oidc/exchange | auth=public | timeout=request | middleware=[WithTimeout(request)] | handler=handlers.OIDC.Exchange
```

## admin (JWT) — `server.Group("/admin", WithJWTAuth, WithProjectID)`  · auth=jwt
No group-level `WithTimeout`; per-route timeout noted per row.

```
GET /admin/playground | auth=jwt | timeout=request | middleware=[WithJWTAuth, WithProjectID, WithTimeout(request)] | handler=handlers.Graphql.Playground.ServeHTTP (closure)
POST /admin/graphql | auth=jwt | timeout=request | middleware=[WithJWTAuth, WithProjectID, WithTimeout(request)] | handler=handlers.Graphql.Graphql.ServeHTTP (closure)
POST /admin/playground/chat | auth=jwt | timeout=llm | middleware=[WithJWTAuth, WithProjectID, WithTimeout(llm), WithSource(playground)] | handler=handlers.Playground.ChatCompletion
GET /admin/requests/:request_id/content | auth=jwt | timeout=request | middleware=[WithJWTAuth, WithProjectID, WithTimeout(request)] | handler=handlers.RequestContent.DownloadRequestContent
GET /admin/requests/:request_id/preview | auth=jwt | timeout=request | middleware=[WithJWTAuth, WithProjectID, WithTimeout(request)] | handler=handlers.RequestPreview.PreviewRequest
POST /admin/codex/oauth/start | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.Codex.StartOAuth
POST /admin/codex/oauth/exchange | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.Codex.Exchange
POST /admin/codex/auth/decode | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.Codex.DecodeAuthJSON
POST /admin/claudecode/oauth/start | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.ClaudeCode.StartOAuth
POST /admin/claudecode/oauth/exchange | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.ClaudeCode.Exchange
POST /admin/antigravity/oauth/start | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.Antigravity.StartOAuth
POST /admin/antigravity/oauth/exchange | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.Antigravity.Exchange
POST /admin/copilot/oauth/start | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.Copilot.StartOAuth
POST /admin/copilot/oauth/poll | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.Copilot.PollOAuth
GET /admin/oidc/link/:provider | auth=jwt | timeout=none | middleware=[WithJWTAuth, WithProjectID] | handler=handlers.OIDC.GetLinkAuthorizeURL
```

## openapi — `server.Group("/openapi", WithIPBlocklist, WithOpenAPIAuth, WithTimeout(RequestTimeout))`  · auth=openapi_service_account · timeout=request

```
POST /openapi/v1/graphql | auth=openapi_service_account | timeout=request | middleware=[WithIPBlocklist, WithOpenAPIAuth, WithTimeout(request)] | handler=handlers.OpenAPIGraphql.Graphql.ServeHTTP (closure)
GET /openapi/v1/playground | auth=openapi_service_account | timeout=request | middleware=[WithIPBlocklist, WithOpenAPIAuth, WithTimeout(request)] | handler=handlers.OpenAPIGraphql.Playground.ServeHTTP (closure)
POST /openapi/webhook/echo | auth=openapi_service_account | timeout=request | middleware=[WithIPBlocklist, WithOpenAPIAuth, WithTimeout(request)] | handler=handlers.System.WebhookEcho
```

## api (LLM) — `server.Group("/", WithTimeout(LLMRequestTimeout), WithIPBlocklist, WithAPIKeyConfig, WithSource(API), WithThread, WithTrace)`  · auth=api_key · timeout=llm

### openai (`apiGroup.Group("/v1")`)

```
POST /v1/chat/completions | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.ChatCompletion
POST /v1/completions | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.Completion
POST /v1/responses/compact | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.CompactResponse
POST /v1/responses | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.CreateResponse
GET /v1/models | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.ListModels
GET /v1/models/*model | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.RetrieveModel
POST /v1/embeddings | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.CreateEmbedding
POST /v1/images/generations | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.CreateImage
POST /v1/images/edits | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.CreateImageEdit
POST /v1/videos | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.CreateVideo
GET /v1/videos/:id | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.GetVideo
DELETE /v1/videos/:id | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.DeleteVideo
POST /v1/audio/speech | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.CreateSpeech
POST /v1/audio/transcriptions | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.CreateTranscription
POST /v1/audio/translations | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.OpenAI.CreateTranslation
POST /v1/messages | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.Anthropic.CreateMessage (OpenAI-compatible Anthropic endpoint)
POST /v1/rerank | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.Jina.Rerank
```

> Note: `/v1/images/variations` is intentionally NOT registered (commented out in Go: "DO NOT SUPPORT IMAGE VARIATION").

### jina (`apiGroup.Group("/jina/v1")`)

```
POST /jina/v1/embeddings | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.Jina.CreateEmbedding
POST /jina/v1/rerank | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.Jina.Rerank
```

### anthropic (`apiGroup.Group("/anthropic/v1")`)

```
POST /anthropic/v1/messages | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.Anthropic.CreateMessage
GET /anthropic/v1/models | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.Anthropic.ListModels
```

### doubao (`apiGroup.Group("/doubao/v3")`)

```
POST /doubao/v3/contents/generations/tasks | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.Doubao.CreateTask
GET /doubao/v3/contents/generations/tasks/:id | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.Doubao.GetTask
DELETE /doubao/v3/contents/generations/tasks/:id | auth=api_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithAPIKeyConfig, WithSource(api), WithThread, WithTrace] | handler=handlers.Doubao.DeleteTask
```

## gemini — two SEPARATE groups, both via `registerGeminiRoutes`  · auth=gemini_key · timeout=llm
Per `RUST-P0-001` S11 the `/gemini/:gemini-api-version/models/*action` and `/v1beta/models/*action` mounts are recorded separately (not merged).

### `server.Group("/gemini/:gemini-api-version", WithTimeout(LLMRequestTimeout), WithIPBlocklist, WithGeminiKeyAuth, WithSource(API), WithThread, WithTrace)`

```
POST /gemini/:gemini-api-version/models/*action | auth=gemini_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithGeminiKeyAuth, WithSource(api), WithThread, WithTrace] | handler=handlers.Gemini.GenerateContent
GET /gemini/:gemini-api-version/models | auth=gemini_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithGeminiKeyAuth, WithSource(api), WithThread, WithTrace] | handler=handlers.Gemini.ListModels
```

### `server.Group("/v1beta", WithTimeout(LLMRequestTimeout), WithIPBlocklist, WithGeminiKeyAuth, WithSource(API), WithThread, WithTrace)` (Gemini alias)

```
POST /v1beta/models/*action | auth=gemini_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithGeminiKeyAuth, WithSource(api), WithThread, WithTrace] | handler=handlers.Gemini.GenerateContent
GET /v1beta/models | auth=gemini_key | timeout=llm | middleware=[WithTimeout(llm), WithIPBlocklist, WithGeminiKeyAuth, WithSource(api), WithThread, WithTrace] | handler=handlers.Gemini.ListModels
```

## CORS (conditional — only when `Config.CORS.Enabled`)

```
OPTIONS *any | auth=public | timeout=none | middleware=[cors] | handler=cors.Handler (CORS preflight; registered via server.OPTIONS("*any", corsHandler))
```
