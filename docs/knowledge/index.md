# Conduit API 项目知识入口

当前项目为 `G:/fubox_API`，图谱 project 为 `conduit-api-main`。先按功能阅读以下入口，再用图谱定位符号；图谱身份、内容绑定、覆盖和查询限制见[索引维护](indexing.md)。旧 Lite/Full、r3 的日期、字段数量和检查记录是历史基线，不代表当前全部能力。

| 需要了解 | 入口 |
| --- | --- |
| 模块及依赖 | [架构](architecture.md) |
| 页面、分组、Key、模型定价与钱包操作 | [前端](frontend.md) |
| 管理接口及治理 | [GraphQL](admin-graphql.md)、[实际 SDL](../../tests/contracts/admin_graphql_schema.graphql) |
| 公共协议与身份 | [HTTP](http-api.md) |
| 授权、计费、持久化与恢复 | [后端](backend.md) |
| 启动、配置及部署 | [基础设施](infrastructure.md) |
| 操作导航 | [操作流程](operations.md) |
| 索引输入及限制 | [索引维护](indexing.md)、[输入清单](source-manifest.md) |

## 当前功能边界

UserGroup 管理访问授权与在线充值倍率；Key 固定一个 ModelGroup，使用该组价格；Project 保留路由、管理及资金范围。旧用户和 Key 迁移后需显式配置分组，不能将未配置视为开放访问。组价格按有序条件 → 组基础价 → 全局价选择，零有效，损坏配置报错；市场只展示获授权 scope 的价格，Project 调整一次，展示选择不改变 Key 的请求分组。

Playground 使用普通 API Key，通过 `GET /v1/models` 和 `POST /v1/chat/completions` 发送真实流式请求。`GET /admin/playground` 是另一项 GraphQL 调试入口，不是聊天传输入口。

在线充值使用默认关闭的易支付参考配置，支持支付宝/微信。固定第三方 reference 的本地 TLS 模拟网关已验证；真实商户和远端 CI 未运行，不能推广为官方或所有 vendor 兼容。返回钱包页面不证明支付成功，只有服务器确认并幂等入账才改变余额。配置入口和安全边界见[后端](backend.md)、[前端钱包](frontend.md)。

结算保留原 estimate/shortfall 策略，不宣称预留是真实 token 数量上界。启用 `group_access` 审批时，全局组直接写入仍返回 `APPROVAL_REQUIRED`，尚无对应审批 commands。旧 raw 基础引用的草稿可打开，保存仍受 `SOURCE_CHANGED` 和包 revision 保护。

## 仍需保留的限制

聊天 fallback 出站只注册五格式；特殊 provider 出站阻断、未注册的非聊天专属出站、multipart JSON 桥、视频本地 GET/DELETE、Dashboard 启动 FixedOffset、未接入 worker 和 OTEL exporter 的具体限制仍见[后端](backend.md)。配置、helper 或 fixture 存在不代表真实提供商已验证。

工程约定见 [AGENTS](../../AGENTS.md)，部署见[部署资料](../production-deployment.md)。本入口不保存任务调度状态。
