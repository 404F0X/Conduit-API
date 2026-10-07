# 操作、流程与证据导航

所有操作均绑定当前[1366源输入](source-manifest.md)。用户操作、API入口和自动业务族口径分别保持；公共根字段、method/path与模板逐项列出，可复用Mermaid但不省略各自步骤。公共功能清单复用静态探索；本轮21项修复另有隔离PG与本地mock回归，未运行真实上游或浏览器业务。

| 操作类别 | 逐项入口与流程 | 数量口径 |
| --- | --- | --- |
| UI页面/布局与动作 | [前端](frontend.md)与各UI-ID recipe | 50page+2layout；组合ID/146根recipe组不是用户动作总数 |
| Admin GraphQL | [254独立根](admin-graphql.md)，每根到recipe/flow/参数/权限 | 105Q149M；229UI链/25API-only；0subscription |
| 客户端模板 | [268文档](frontend-client-operations.md)，每文档所有根映射 | 119Q149M；266匹配/2残留，多根不截首 |
| HTTP | [55method/path](http-api.md#http-methods) | 53注册55组合；不含static/metrics/自动OPTIONS |
| 静态SPA与metrics | [静态](http-api.md#flow-static)、[观测](infrastructure.md#flow-observe) | fallback与独立listener另列 |
| OpenAPI服务帐号管理 | [5根](backend.md#flow-openapi) | 2Q3M；项目/scopes/one-of标识检查 |
| CLI、部署、开发工具 | [基础设施28项](infrastructure.md) | 原生CLI9含默认启动；其他为部署/检查使用操作 |
| 执行、自动、管理运维族 | [后端](backend.md) | RUNTIME10/AUTO13/OPS8；私有helper不是独立公开入口 |

机器证据：[frontend-operations-a5.json](../../.codex-workflow/runs/project-exploration-20261005-acceptance/artifacts/index-docs/frontend-operations-a5.json)含254root+268client全绑定；[backend-operations-a5.json](../../.codex-workflow/runs/project-exploration-20261005-acceptance/artifacts/index-docs/backend-operations-a5.json)含55http及每OpenAPI/自动/执行/ops条目；旧http-source-routes.json含53注册55行和精确源行；[frontend-recipe-bindings-a5.json](../../.codex-workflow/runs/project-exploration-20261005-acceptance/artifacts/index-docs/frontend-recipe-bindings-a5.json)含254→146recipe。修正后的机器绑定位于[当前证据目录](../../.codex-workflow/runs/project-exploration-20261005-acceptance/artifacts/index-docs/)；原始研究批次仍位于[旧证据目录](../../.codex-workflow/runs/project-exploration-20261005/artifacts/index-docs/)，其版本/来源批次与哈希在最终候选manifest。

实际检查记录：本地Markdown链接/anchor、所有Mermaid语法解析、各方法路径/根字段/模板/目录计数、source-input SHA和图持久文件匹配。旧探索未运行工程门禁；本轮Rust、前端与隔离PG实际记录见[修复检查](execution-retrospective.md#repair-checks)。真实provider/浏览器E2E未运行。独立验证对最终冻结修复候选给出结论。


## 修复后的跨层运行导航

公共操作及schema字段数量未新增。Owner授予见[平台权限边界](admin-graphql.md#repair-owner-grant)，登录存储/密码恢复/用户保存错误/全局请求返回见[当前前端操作](frontend.md)。后台机制对应已有请求/设置/运维功能：

| 问题ID | 操作或触发 | 用户/运维处理与流程 |
| --- | --- | --- |
| F-01/F-02/F-04/F-05/F-14/F-15 | 推理stream、客户端Stop/断开、请求观测 | [stream与活动期限](backend.md#B-RUNTIME-07)、[受监督关停](backend.md#flow-supervision)；查看父子状态和最终指标 |
| F-03 | 成功usage初次写入或PG故障 | [持久输入/重放](backend.md#flow-durable-usage)、[部署恢复](infrastructure.md#usage-recovery-deployment) |
| F-06/F-16 | 自动/手动备份、retentionDays | [快照与保留](backend.md#B-RECOVERY04)；检查lastBackupAt/Error，scheduled失败可见、成功清错；Skipped释放日期claim，同日可重试；不删除未知对象 |
| F-07/F-08/F-09 | 管理设置保存、并发后台状态回写 | [设置消费者](backend.md#flow-settings)及[缓存读提交语义](backend.md#B-CACHE01)；DB提交与cache告警分开 |
| F-10/F-11 | GC、内容下载与视频归档 | [删除队列与tombstone](backend.md#flow-artifact-delete)、[视频公平扫描](backend.md#B-RECOVERY02) |
| F-12/F-13 | 多实例同due业务、启动失败/关闭 | [持久认领与监督](backend.md#B-RECOVERY03) |

本轮检查不是提供商能力或生产部署验收，实际门禁/资源证据见[修复记录](execution-retrospective.md#repair-checks)。
