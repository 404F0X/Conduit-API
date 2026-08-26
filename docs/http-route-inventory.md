# HTTP Route 静态清单

基准：2026-08-19 当前 `crates/conduit-http/src/router.rs`。路径会经过可配置 `base_path`；下表写未加前缀的逻辑路径。注册成功只证明路由进入对应 handler，不等于所有 provider、输入变体和计费分支已完成生产验收。

## Public

| 方法 | 路径 | Handler | 用途 |
|---|---|---|---|
| GET | `/health` | `health::health` | 存活检查；是否挂根路径受 base path 条件影响 |
| GET | `/api/system/version` | `admin_handlers::system_version` | 版本 |
| GET | `/admin/system/status` | `system_handlers::get_system_status` | 初始化状态 |
| POST | `/admin/system/initialize` | `system_handlers::initialize_system` | 首次初始化 |
| POST | `/admin/auth/signin` | `auth_handlers::sign_in` | 密码登录 |
| POST | `/admin/auth/signup` | `auth_handlers::sign_up` | 注册 |
| GET | `/favicon` | `system_handlers::get_favicon` | 品牌图标 |
| GET | `/oauth/oidc/providers` | `oidc_handlers::get_providers` | OIDC provider 列表 |
| GET | `/oauth/oidc/authorize/{provider}` | `oidc_handlers::get_authorize_url` | OIDC 登录跳转 |
| GET | `/oauth/oidc/callback` | `oidc_handlers::callback` | OIDC 回调 |
| GET | `/oauth/oidc/callback/{provider}` | `oidc_handlers::callback_with_provider` | 带 provider 回调 |
| POST | `/oauth/oidc/exchange` | `oidc_handlers::exchange` | 前端 code 交换 |

## JWT Admin

| 方法 | 路径 | Handler | 用途 |
|---|---|---|---|
| GET | `/admin/oidc/link/{provider}` | `oidc_handlers::get_link_authorize_url` | 当前用户绑定 OIDC |
| POST | `/admin/{provider}/oauth/start` | `oauth_handlers::start_oauth` | 渠道/provider OAuth 启动 |
| POST | `/admin/{provider}/oauth/exchange` | `oauth_handlers::exchange` | 渠道/provider OAuth 交换 |
| POST | `/admin/copilot/oauth/poll` | `oauth_handlers::poll_oauth` | Copilot device flow 轮询 |
| POST | `/admin/codex/auth/decode` | `oauth_handlers::decode_auth_json` | Codex auth JSON 解码 |
| GET | `/admin/requests/{request_id}/content` | `request_content_handlers::download_request_content` | Project 范围请求内容 |
| GET | `/admin/requests/{request_id}/preview` | `request_preview_handlers::preview_request` | Project 范围实时/静态预览 |
| POST | `/admin/graphql` | `graphql_handlers::graphql_handler` | 管理 GraphQL |
| GET | `/admin/playground` | `graphql_handlers::graphql_playground` | GraphQL Playground |

## Service Account / Internal

| 方法 | 路径 | 鉴权 | Handler |
|---|---|---|---|
| POST | `/openapi/v1/graphql` | 数据库有效 service-account API Key；Project 范围 | `openapi_graphql_handlers::graphql_handler` |
| POST | `/openapi/webhook/echo` | service-account API Key | `webhook_handlers::webhook_echo` |
| POST | `/internal/v1/graphql` | service-account API Key + `system:admin`；系统级 Owner authority | `graphql_handlers::internal_graphql_handler` |

## LLM API（API Key）

| 方法 | 路径 | Handler | 协议/请求类型 |
|---|---|---|---|
| GET | `/v1/models` | `openai_handlers::list_models` | OpenAI 模型列表 |
| GET | `/v1/models/{*model}` | `openai_handlers::retrieve_model` | OpenAI 模型详情，支持含 `/` 的 ID |
| GET | `/anthropic/v1/models` | `anthropic_handlers::list_models` | Anthropic 模型列表 |
| GET | `/v1beta/models` | `gemini_handlers::list_models` | Gemini 模型列表 |
| POST | `/v1beta/models/{model_action}` | `gemini_handlers::generate_content` | Gemini generate/stream action |
| GET | `/gemini/{gemini_api_version}/models` | `gemini_handlers::list_models` | 带版本前缀的 Gemini 模型列表 |
| POST | `/gemini/{gemini_api_version}/models/{model_action}` | `gemini_handlers::generate_content` | 带版本前缀的 Gemini generate/stream action |
| POST | `/v1/messages` | `anthropic_handlers::create_message` | Anthropic Messages |
| POST | `/v1/messages/count_tokens` | `anthropic_handlers::count_message_tokens` | Anthropic token count |
| POST | `/anthropic/v1/messages` | `anthropic_handlers::create_message` | Anthropic 前缀兼容 |
| POST | `/anthropic/v1/messages/count_tokens` | `anthropic_handlers::count_message_tokens` | Anthropic 前缀 token count |
| POST | `/v1/chat/completions` | `openai_handlers::create_chat_completion` | OpenAI Chat Completions |
| POST | `/v1/responses` | `openai_handlers::create_response` | OpenAI Responses |
| POST | `/v1/completions` | `openai_handlers::create_completion` | OpenAI Legacy Completions |
| POST | `/v1/responses/compact` | `openai_handlers::create_compact_response` | Responses compact |
| POST | `/v1/rerank` | `openai_handlers::create_jina_rerank` | Jina rerank 兼容 |
| POST | `/jina/v1/rerank` | `openai_handlers::create_jina_rerank` | Jina rerank 前缀 |
| POST | `/jina/v1/embeddings` | `openai_handlers::create_jina_embedding` | Jina embedding |
| POST | `/doubao/v3/contents/generations/tasks` | `openai_handlers::create_doubao_task` | 豆包视频任务创建 |
| GET | `/doubao/v3/contents/generations/tasks/{id}` | `openai_handlers::get_video` | 豆包任务查询 |
| DELETE | `/doubao/v3/contents/generations/tasks/{id}` | `openai_handlers::delete_video` | 豆包任务删除 |
| POST | `/v1/embeddings` | `openai_handlers::create_embedding` | OpenAI Embeddings |
| POST | `/v1/audio/speech` | `openai_handlers::create_speech` | OpenAI Speech |
| POST | `/v1/videos` | `openai_handlers::create_video` | 视频任务创建 |
| GET | `/v1/videos/{id}` | `openai_handlers::get_video` | 视频任务查询 |
| DELETE | `/v1/videos/{id}` | `openai_handlers::delete_video` | 视频任务删除 |
| POST | `/v1/images/generations` | `openai_handlers::create_image` | 图片生成 |
| POST | `/v1/images/edits` | `openai_handlers::create_image_edit` | 图片编辑 |
| POST | `/v1/audio/transcriptions` | `openai_handlers::create_transcription` | 音频转录 |
| POST | `/v1/audio/translations` | `openai_handlers::create_translation` | 音频翻译 |

## 运行契约注意事项

- `/admin/requests/{id}/preview` 的实时 replay buffer 当前是进程内存，代码契约明确为 single-instance-only；多实例部署需要粘性路由或共享事件后端。
- `/internal/v1/graphql` 与 `/openapi/v1/graphql` 不是同一权限层：前者是平台管理员自动化，后者是 Project service account。
- 所有 LLM 路由共享 API Key 中间件，但最终可用范围仍应取 Project effective access 与 Key profile 的交集。
- `asset_fallback_handler` 是 SPA 静态资源 fallback，不是业务 API。
