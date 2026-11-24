# 项目索引维护

## 当前身份与内容绑定（2026-11-04）

根目录 `G:/fubox_API`，project `conduit-api-main`；本机 [.codebase-memory/client.json](../../.codebase-memory/client.json) 指向固定 native `codebase-memory-mcp 0.11.0`。本次使用现有脚本和排除规则，无安装、升级或配置修改。旧图的内容校验明确 STALE（131 个输入变化）；已完成授权 full refresh。

当前 [artifact](../../.codebase-memory/artifact.json) indexed_at 为 `2026-11-04T19:18:17Z`，持久图 53391 nodes / 235929 edges；native status 的 indexed_at `19:18:10Z` 是本轮内部索引时间，两字段语义不作相等要求。[freshness](../../.codebase-memory/freshness.json) 绑定 1443 个非 Markdown 输入，input SHA256 `c730a5e5b48c1f44d05c656dacb32f1d6a543f7b60b29ae12455f151cc41b2cb`，并绑定 graph SHA/root/project/generation。HEAD 仅是参考，不能证明 dirty 源内容一致。refresh 内部 before/after 内容一致保护已通过；源码清单只共享这一份，不另造全树清单。

**范围限制**：上述 freshness 排除 Markdown；图中确有 Markdown File 节点。本轮文档在建图后维护，文档另作文件内容绑定，不称图内 Markdown 节点已更新。查当前知识请直接读取本目录，不把图中的旧文档片段当当前事实。非 Markdown 清单与 File 数量口径不同，节点数量不是覆盖或正确性证明。

## 实际查询与覆盖

机器证据位于 [.codebase-memory/evidence/knowledge-20261104](../../.codebase-memory/evidence/knowledge-20261104/)；[本轮 verification](../../.codebase-memory/verification.json) 汇总当前输入和独立验收状态。Rust `save_user_group` 搜索命中 resolver / trait / PgAdapter；前端 `streamChat`、`useAssignableModelGroups` 命中。两端 snippet 返回 full 源码及路径/行号；真实前端 trace 为 `send → streamChat`，与现源调用一致。Rust resolver trace 能看到 `global` / `services`，并未解析运行时 trait dispatch 到 PgAdapter；不可把缺少该边当没有调用。

[关键文件查询](../../.codebase-memory/evidence/knowledge-20261104/coverage.json) 确认 groups/payments 的 GraphQL、Postgres adapter、前端组页面、retail_price_plan 和 migrations `000040` / `000041` 都有 File 节点。wallet/market/Playground 目录另有查询证据。File 命中只证明存在，不保证其中每个符号/SQL 约束都完整解析。

完整 [status](../../.codebase-memory/evidence/knowledge-20261104/status-actual.json) 保留 **16 partial / 0 unusable / 0 skipped** 的文件及行范围；partial 包含 package-detail-page.tsx、CSS 与 14 个 SQL 文件，特别是 `000041_payment_topups.sql` 的 3–9、17–19、27–28、53–79 行。对应构造须直接读源/迁移及行为证据，不能从空查询推断不存在。54 个 not_indexed 文件是规则排除，27 个目录另列；它们不是 parser 失败。完整字段未截断，refresh 初始摘要和首次 File 全表查询均有截断，不能代替完整覆盖。

## 完整 File 路径差集补证（2026-11-05）

完整导出 [File 集合](../../.codebase-memory/evidence/knowledge-20261104/resume-20261105/file-paths.json) 返回 1440 个唯一路径，total_relation=eq、has_more=false、truncated=false；[逐路径差集](../../.codebase-memory/evidence/knowledge-20261104/resume-20261105/file-set-difference.json) 与原唯一 1443 项非 Markdown 清单比较：交集 1401、清单独有 42、图独有 39（全部 Markdown）。旧 collect 的单行字符串有截短，旧两页各 200 项也不是全集；本次导出替代它们的全集证明用途，原记录保留。

crates 为清单 486 / 图 470：缺的 16 项恰为 15 个 transformer JSONL 流 fixture 和一个 favicon，无 Rust 源路径缺失。frontend 为清单 838 / 图 826：清单独有 13 项（环境示例、点配置、NOTICE、package.json、tsconfig.json、3 个图标及中英文 settings.json），图独有 locales/README.md 一项。其余清单独有 13 项为根级配置、Cargo.lock、许可文本及许可证模板；全部具体路径见差集。不能把所有差异都归入 status 的 54 not_indexed：已知 .env.example 是 cbmignore、图标是 ignored-suffix；其余未取得逐项 native 排除原因，不据此宣称 parser 失败或擅改规则。清单独有项有内容绑定，但没有 File/符号覆盖证明，应直接读取。

groups/payments 的 resolver 与 Postgres wiring、retail_price_plan.rs、Playground index/chat-client（含测试）、迁移 000040/000041 均逐路径命中；这补全 File 存在性证据，不替代既有 snippet/trace 和 partial 限制。本次 query 经原 wrapper 的完整内容检查后 exit 0：仍为原 1443 项 input SHA 和同代 graph SHA；没有 refresh，也未新增全树清单。先核对原 9 文档 hash 全部一致后，仅更新本索引说明并重新绑定文档。图内 Markdown 仍不保证当前，以上非 Markdown freshness 不推广到它们。

## 查询、维护与写入边界

MCP 本轮没有暴露图工具。优先 `search_graph → trace_path → get_code_snippet`；图陈旧、覆盖不足或无法确认只读时，按问题使用有界 `rg`。native CLI 查询可能自动维护 runtime/cache，wrapper 也会创建并删除临时参数文件，因此只有明确索引 owner 执行 query/status/refresh；严格只读读已导出的机器证据。空结果、no_recorded_issue、同 HEAD 或原生 best-effort metadata 均不能代替内容校验。

```powershell
python scripts/knowledge.py check
python scripts/knowledge.py refresh
python scripts/knowledge.py query search_graph --args-file <UTF-8 JSON 文件>
```

`refresh` 绑定 root/project，full index 后校验源输入未变，再原子发布 freshness。`check` 校验实际输入 SHA、graph SHA 和 generation；`query` 先 check。native 在中文路径读取 args-file 失败，本轮保留原错误；现有 wrapper 在 G 工作树内创建 ASCII 路径参数文件可以正常工作，不必修改脚本。

本 project 专用 runtime/cache 位于 `C:/ConduitKnowledge-codex/{runtime,cache}`；持久图在本工作树 `.codebase-memory`。维护不改全局 Agent 配置、其他项目 cache 或业务数据库。遵守 [.cbmignore](../../.cbmignore) 与 [wrapper](../../scripts/knowledge.py) 的实际规则：研究副本 SillyTavern/axonhub、worktrees、依赖 node_modules/target、dist、测试报告、任务账本、runtime/journal、密钥/真实配置等排除；具体目录与文件以完整 status 为准。wrapper 内容清单排除与 native File 集合不完全相同，不能互换。

旧 10 月索引/验证产物属于历史代，已保存本轮 before 证据，不作当前通过。Rust 宏、trait dispatch、TSX 回调、动态 SQL 和解析恢复均需源确认；本次更新不重跑产品工程门禁，也不声称真实 provider 或商户验证。
