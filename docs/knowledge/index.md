# Conduit API 项目知识入口

唯一源码工作树为G:/fubox_API。2026-10-05的全量静态探索保留为公共操作基线；2026-10-06至07的21项工程修复同步实现、配置、部署约定和必要流程。运行支持以当前生产wiring为准；隔离PG/mock回归、项目门禁与未运行条件见[实际检查](execution-retrospective.md#repair-checks)。

## 从功能进入

| 读者需要 | 权威入口 | 覆盖口径 |
| --- | --- | --- |
| 全部目录、模块及上下游 | [架构与全模块](architecture.md) | 17crate/31feature目录及全部输入目录 |
| 页面、用户动作与权限/失败步骤 | [前端操作](frontend.md) | 50页面+2布局，真实/本地/占位/缺口分别说明 |
| 管理GraphQL API | [254根字段](admin-graphql.md) | 105Q149M；25API-only/229当前UI链；JWT必需 |
| 前端所有API模板与根映射 | [268客户端文档](frontend-client-operations.md) | 119Q149M；两旧常量不支持，多根全保留 |
| 公共HTTP与推理协议 | [逐method/path](http-api.md) | 53注册55method-path；另static/独立metrics |
| 后端、持久化与真实消费者 | [执行/计费/存储/自动任务](backend.md) | OpenAPI5根及执行、自动、运维功能族 |
| 持久计量、删除、关闭、备份恢复 | [修复运行流程](backend.md#B-RECOVERY01) | WAL/receipt、tombstone、PG claim、监督与一致快照 |
| 启动/配置/CLI/部署/测试/CI | [基础设施](infrastructure.md) | CLI9含默认启动；新增journal持久卷约定 |
| 索引缓存、查询与更新 | [索引维护](indexing.md) | 当前身份/差集与历史coverage分开记录 |
| 每文件内容基线 | [源码输入哈希](source-manifest.md) | 非Markdown源；File节点与输入数口径不同 |
| 全部操作ID与来源/检查 | [操作导航](operations.md) | 按入口区分，不把族计数冒充按钮数 |
| 工作流经验与检查边界 | [执行反思](execution-retrospective.md) | 实际检查、历史失败、资源状态和独立验收边界 |

## 当前源码与图谱身份

HEAD=`3f1dbb00cd6c4d6e2d3d7478591d8e24b0ec1e12`；当前非Markdown输入1366文件，SHA256=`4c2a73a17e7cd18da0300d75c171e7dbb598eae33a6592e2e98793e79eeca3f0`。HEAD和内容指纹一起绑定，不能只凭commit排除dirty变化。

已发布project=`conduit-api-main`，schema=2；graph SHA256=`5eedb6bbd2c525fdc03eaedb3ac8e5d455289e108c655f2c2b0318a456d1d4bf`，生成时间`2026-10-02T20:15:28Z`，File=1344。当前图freshness为 **stale**。当前1366源输入对应的21项修复与适用工程门禁已获唯一独立审查APPROVE；全局仍缺当前权威索引。v6在任务目录ACL检查失败，v7 pipeline失败原因当时UNKNOWN；a3/v8以新私有daemon/worker继承CBM_PROFILE=1保留详细日志，确认仓库artifact导出写临时文件返回errno28（ENOSPC）。G旧三文件SHA一致，无需恢复；失败后的C缓存未发布为权威图，停止调用并等待G空间状态修复。[当前执行证据](../../.codex-workflow/runs/quality-repair-20261006/artifacts/index-failure-a3-v8.json)记录具体阶段，不能用旧图片段证明新实现。

当前输入与已发布图的差集：52输入缺File，反向30节点不在源码freshness口径；[完整当前差集](indexing.md#current-source-graph-limits)。历史1357/1344、42/29与28缺口是原探索证据，不冒充当前计数。Markdown独立冻结内容SHA，源freshness不保证其当前片段。

新问题按search_graph → trace_path → get_code_snippet → query_graph/get_architecture；文本/配置或明确图缺口按AGENTS有界读源。查询可能auto-refresh，没有保证不写的开关；严格只读叶子读取维护者保存snapshot，只有明确持有索引写入权的维护者刷新。

## 保留的生产限制

聊天fallback出站只注册五格式，anthropic_gcp/gemini_vertex/claudecode/antigravity/codex/github_copilot明确阻断；embedding/rerank/image/audio/video/legacy专属出站未注册，multipart三端点走JSON桥。视频GET/DELETE仅本地；UI聊天Playground缺生产POST。Dashboard时区仅启动FixedOffset、两个worker未启用、OTEL无exporter。这些能力未在21项修复中扩展。自动备份retentionDays已经接入严格前缀清理；其失败与重试步骤见[备份](backend.md#B-RECOVERY04)。配置/helper/fixture存在不代替真实提供商验证。

[AGENTS规范](../../AGENTS.md)与[原部署资料](../production-deployment.md)提供工程及运行约定。新journal部署条件见[持久计量部署](infrastructure.md#usage-recovery-deployment)。
