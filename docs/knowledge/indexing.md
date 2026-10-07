# 项目索引维护

当前内容身份以[源码清单](source-manifest.md)和本节为准，下方原探索JSON/42与29表是历史证据。

project=`conduit-api-main`，当前源码1366/`4c2a73a17e7cd18da0300d75c171e7dbb598eae33a6592e2e98793e79eeca3f0`，已发布图1344/`5eedb6bbd2c525fdc03eaedb3ac8e5d455289e108c655f2c2b0318a456d1d4bf`；freshness=**stale**。旧图未刷新，查询可能写缓存，不能将旧源码片段用于证明新实现。G旧图与SHA一致C备份保留；a2 v6 CLI端点ACL检查失败，v7修复ACL后pipeline失败，旧报告的底层原因仍UNKNOWN。a3/v8新私有进程使用CBM_PROFILE=1，保留worker日志，实际在artifact_export/write_artifact/write_temp返回errno28（ENOSPC）；三份旧G图SHA一致无需恢复，失败C缓存不是权威发布。[v8执行报告](../../.codex-workflow/runs/quality-repair-20261006/artifacts/index-execution-a3-v8.json)与[详细失败证据](../../.codex-workflow/runs/quality-repair-20261006/artifacts/index-failure-a3-v8.json)分别记录调用结果和具体阶段。仅执行一次，停止刷新；等待用户增加G可用空间及新的实际预检/明确授权，不改排除规则压缩功能范围，不重试被拒绝的清理。新压缩artifact实际大小未输出，不能据旧16.45MB图或某固定阈值宣称空间足够。

默认journal/runtime/依赖/凭据排除索引与source freshness；自定义usage_recovery目录必须同时加入`.cbmignore`及维护输入排除。journal只记录白名单，禁止把WAL或ready/quarantine内容当图源读取。Markdown不在source freshness中，问题定位须直接读取当前MD并核内容SHA。

<a id="current-source-graph-limits"></a>
## 当前完整输入/File差集

输入缺File=52；反向File不在输入=30；导出未截断。[机器差集](../../.codex-workflow/runs/quality-repair-20261006/logs/a2/source-graph-limits.json)。

| 当前输入缺File |
| --- |
| [.cargo/audit.toml](../../.cargo/audit.toml) |
| [.cargo/config.toml](../../.cargo/config.toml) |
| [.cbmignore](../../.cbmignore) |
| [.dockerignore](../../.dockerignore) |
| [.gitignore](../../.gitignore) |
| [.node-version](../../.node-version) |
| [Cargo.lock](../../Cargo.lock) |
| [LICENSE](../../LICENSE) |
| [LICENSES/LGPL-3.0-only.txt](../../LICENSES/LGPL-3.0-only.txt) |
| [LICENSES/POSTGRESQL_WINDOWS_THIRD_PARTY_LICENSES.txt](../../LICENSES/POSTGRESQL_WINDOWS_THIRD_PARTY_LICENSES.txt) |
| [LICENSES/PostgreSQL.txt](../../LICENSES/PostgreSQL.txt) |
| [NOTICE](../../NOTICE) |
| [crates/conduit-bin/src/artifact_cleanup.rs](../../crates/conduit-bin/src/artifact_cleanup.rs) |
| [crates/conduit-bin/src/maintenance_claim.rs](../../crates/conduit-bin/src/maintenance_claim.rs) |
| [crates/conduit-bin/src/repair_regressions.rs](../../crates/conduit-bin/src/repair_regressions.rs) |
| [crates/conduit-bin/src/usage_recovery.rs](../../crates/conduit-bin/src/usage_recovery.rs) |
| [crates/conduit-http/assets/favicon.ico](../../crates/conduit-http/assets/favicon.ico) |
| [crates/conduit-scheduler/src/tasks.rs](../../crates/conduit-scheduler/src/tasks.rs) |
| [crates/conduit-transformers/tests/fixtures/anthropic/anthropic-error.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/anthropic-error.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/anthropic/anthropic-stop.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/anthropic-stop.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/anthropic/anthropic-think.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/anthropic-think.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/anthropic/anthropic-tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/anthropic-tool.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/anthropic/llm-stop.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/llm-stop.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/anthropic/llm-think.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/llm-think.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/anthropic/llm-tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/llm-tool.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/openai/deepseek-reasoninig.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/deepseek-reasoninig.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_2.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_2.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_3.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_3.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/openai/openai-parallel_multiple_tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-parallel_multiple_tool.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/openai/openai-stop.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-stop.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/openai/openai-tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-tool.stream.jsonl) |
| [crates/conduit-transformers/tests/fixtures/openai/openai-tool_2.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-tool_2.stream.jsonl) |
| [frontend/.env.example](../../frontend/.env.example) |
| [frontend/.gitignore](../../frontend/.gitignore) |
| [frontend/.npmrc](../../frontend/.npmrc) |
| [frontend/.prettierignore](../../frontend/.prettierignore) |
| [frontend/.prettierrc](../../frontend/.prettierrc) |
| [frontend/NOTICE](../../frontend/NOTICE) |
| [frontend/package.json](../../frontend/package.json) |
| [frontend/public/favicon.ico](../../frontend/public/favicon.ico) |
| [frontend/public/logo.svg](../../frontend/public/logo.svg) |
| [frontend/src/assets/vite.svg](../../frontend/src/assets/vite.svg) |
| [frontend/src/features/users/data/save-error.test.mjs](../../frontend/src/features/users/data/save-error.test.mjs) |
| [frontend/src/features/users/data/save-error.ts](../../frontend/src/features/users/data/save-error.ts) |
| [frontend/src/locales/en/settings.json](../../frontend/src/locales/en/settings.json) |
| [frontend/src/locales/zh-CN/settings.json](../../frontend/src/locales/zh-CN/settings.json) |
| [frontend/src/stores/auth-storage.test.mjs](../../frontend/src/stores/auth-storage.test.mjs) |
| [frontend/src/stores/auth-storage.ts](../../frontend/src/stores/auth-storage.ts) |
| [frontend/tsconfig.json](../../frontend/tsconfig.json) |
| [migrations/postgres/000036_recovery_lifecycle.sql](../../migrations/postgres/000036_recovery_lifecycle.sql) |
| [scripts/licenses/rust-third-party.hbs](../../scripts/licenses/rust-third-party.hbs) |

| File不在当前源freshness |
| --- |
| `.github/pull_request_template.md` |
| `AGENTS.md` |
| `CODE_OF_CONDUCT.md` |
| `CONTRIBUTING.md` |
| `LICENSING.md` |
| `README.md` |
| `README_CN.md` |
| `RELEASE_GATES.md` |
| `RELINKING.md` |
| `ROADMAP.md` |
| `SECURITY.md` |
| `docs/domain-model.md` |
| `docs/frontend-graphql-operation-inventory.md` |
| `docs/http-route-inventory.md` |
| `docs/internal-admin-api.md` |
| `docs/knowledge/index.md` |
| `docs/knowledge/indexing.md` |
| `docs/postgres-database-scope.md` |
| `docs/postgres-performance-baseline.md` |
| `docs/production-deployment.md` |
| `docs/project-access-unification.md` |
| `frontend/src/features/auth/forgot-password/components/forgot-password-form.tsx` |
| `frontend/src/locales/README.md` |
| `migrations/INVENTORY.md` |
| `migrations/postgres/README.md` |
| `scripts/e2e/README.md` |
| `scripts/provider-compatibility/README.md` |
| `tests/contracts/README.md` |
| `tests/contracts/llm_cases/README.md` |
| `tests/contracts/routes_snapshot.md` |

<!-- historical-index-evidence -->
# 历史索引证据：2026-10-05

当前持久图由既有codebase-memory-mcp 0.11.0生成。项目自有[scripts/knowledge.py](../../scripts/knowledge.py)核对身份与非文档输入并代理工具；不将图工具缓存当产品PostgreSQL。当前工作树、版本和完整源清单见[入口](index.md)。此段描述2026-10-05原探索，不描述2026-10-07修复后的源码。

## 持久位置和刷新

`.codebase-memory/client.json`保存root/project/command/runtime_dir/cache_dir；`.codebase-memory/artifact.json`记录project/schema/生成信息；`freshness.json`记录输入内容与图指纹；`graph.db.zst`是可保留的压缩持久图。客户端当前指向C:/ConduitKnowledge-codex/runtime和cache；这是本机配置事实，换机器按当地安装和权限设置，不照抄个人路径、不写凭据。wrapper将runtime/cache覆盖一致传给native CLI。

```powershell
python -X utf8 scripts/knowledge.py status
python -X utf8 scripts/knowledge.py check
# 只有明确持有索引写入权的维护者可刷新
python -X utf8 scripts/knowledge.py refresh
```

refresh按排除规则生成并持久化，发布前后核对源指纹；相关输入变化或project错配则不能把旧图标fresh。输入含Git可见非Markdown源、契约、配置、Docker和CI，排除研究/worktree/依赖/输出/runtime/secret/task；新Markdown导航不强迫重新建产品图。full不等parser全语法/调用边全覆盖。

## 查询是否会写

工具将查询标为自动刷新索引，不能称严格只读API。当前CLI wrapper的query还会在.codebase-memory暂存参数；所有可能写缓存的查询由唯一索引维护者执行，严格只读研究者只读取已经保存的JSON。当前受控查询前后检查published graph/artifact/freshness哈希与源输入保持一致；这证明这些文件没变，不能证明引擎runtime内部完全无写入。

```powershell
# 维护者用UTF8参数文件；name_pattern regex，file_pattern SQL LIKE %
python -X utf8 scripts/knowledge.py query search_graph --args-file .codebase-memory/query.json
```

拒绝借参数跨project；分页核对returned/truncated/has_more。查询入口支持search_graph/trace_path/get_code_snippet/query_graph/get_architecture/check_index_coverage，参数以已安装工具帮助为准。

严格只读复用的当前snapshot位于本次授权artifact：files.json、architecture.json、coverage.json、frontend-functions.json、router-source.json、cli-start.json、cli-trace.json、excluded.json。报告及受控查询脚本见[证据目录](../../.codex-workflow/runs/project-exploration-20261005/artifacts/index-docs/)；其他叶子发出具体查询请求，维护者保存再提供路径。任务状态留Root唯一账本，知识包不复制账本。

<a id="coverage"></a>
## 历史一请求、6scope与28缺口记录

File1344未截断，frontend File778/admin49；frontend/admin Function/Method/Struct/Class5058完整页。11项parse_partial为1CSS+10SQL；其余scope的无问题标记也只是best-effort，并不证明完整。Rust动态trait dispatch、同名方法heuristic、宏、JS模板/TSX事件和生产依赖注入须具体源核实。当前两代表片段cli-start与前端About逐字匹配对应源；排除目录query返回0只证明当前索引中无这些File节点。

完整工具覆盖报告（保留range/detail与警告，不改写为通过）：

```json
{
  "project": "conduit-api-main",
  "signal": "best_effort",
  "indexed_at": "2026-10-02T20:15:23Z",
  "metadata": {
    "generation": "2026-10-02T20:15:23Z",
    "index_mode": "full",
    "recorded_at": "2026-10-02T20:15:24Z",
    "recording_status": "complete",
    "ignored_files_stored": 53,
    "ignored_files_total": 53,
    "hash_records_complete": true,
    "coverage_version": 3,
    "generation_matches": true
  },
  "paths": [],
  "path_total": 0,
  "path_returned": 0,
  "path_has_more": false,
  "scopes": [
    {
      "requested_scope": "crates/",
      "scope": "crates",
      "total": 1,
      "returned": 1,
      "truncated": false,
      "entries": [
        {
          "path": "crates/conduit-http/assets/favicon.ico",
          "kind": "not_indexed_file",
          "detail": "ignored-suffix"
        }
      ],
      "status": "known_gaps"
    },
    {
      "requested_scope": "frontend/",
      "scope": "frontend",
      "total": 11,
      "returned": 11,
      "truncated": false,
      "entries": [
        {
          "path": "frontend/.env.development",
          "kind": "not_indexed_file",
          "detail": "gitignore"
        },
        {
          "path": "frontend/.env.example",
          "kind": "not_indexed_file",
          "detail": "cbmignore"
        },
        {
          "path": "frontend/.tanstack/tmp",
          "kind": "not_indexed_dir",
          "detail": "excluded subtree"
        },
        {
          "path": "frontend/dist",
          "kind": "not_indexed_dir",
          "detail": "excluded subtree"
        },
        {
          "path": "frontend/node_modules",
          "kind": "not_indexed_dir",
          "detail": "excluded subtree"
        },
        {
          "path": "frontend/package-lock.json",
          "kind": "not_indexed_file",
          "detail": "gitignore"
        },
        {
          "path": "frontend/playwright-report",
          "kind": "not_indexed_dir",
          "detail": "excluded subtree"
        },
        {
          "path": "frontend/public/favicon.ico",
          "kind": "not_indexed_file",
          "detail": "ignored-suffix"
        },
        {
          "path": "frontend/public/logo.svg",
          "kind": "not_indexed_file",
          "detail": "ignored-suffix"
        },
        {
          "path": "frontend/src/assets/vite.svg",
          "kind": "not_indexed_file",
          "detail": "ignored-suffix"
        },
        {
          "path": "frontend/src/index.css",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 4,
              "end": 4
            },
            {
              "start": 4,
              "end": 6
            },
            {
              "start": 1099,
              "end": 1099
            }
          ],
          "detail": "4-4,4-6,1099-1099"
        }
      ],
      "status": "known_gaps"
    },
    {
      "requested_scope": "scripts/",
      "scope": "scripts",
      "total": 5,
      "returned": 5,
      "truncated": false,
      "entries": [
        {
          "path": "scripts/audit",
          "kind": "not_indexed_dir",
          "detail": "excluded subtree"
        },
        {
          "path": "scripts/contract",
          "kind": "not_indexed_dir",
          "detail": "excluded subtree"
        },
        {
          "path": "scripts/index",
          "kind": "not_indexed_dir",
          "detail": "excluded subtree"
        },
        {
          "path": "scripts/migration",
          "kind": "not_indexed_dir",
          "detail": "excluded subtree"
        },
        {
          "path": "scripts/rust/shadow_compare.sh",
          "kind": "not_indexed_file",
          "detail": "gitignore"
        }
      ],
      "status": "known_gaps"
    },
    {
      "requested_scope": "migrations/",
      "scope": "migrations",
      "total": 10,
      "returned": 10,
      "truncated": false,
      "entries": [
        {
          "path": "migrations/postgres/000001_initial.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 52,
              "end": 55
            },
            {
              "start": 265,
              "end": 267
            }
          ],
          "detail": "52-55,265-267"
        },
        {
          "path": "migrations/postgres/000003_balance_subscription_v1.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 22,
              "end": 23
            },
            {
              "start": 24,
              "end": 25
            },
            {
              "start": 27,
              "end": 28
            }
          ],
          "detail": "22-23,24-25,27-28"
        },
        {
          "path": "migrations/postgres/000006_subscription_project_grants.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 76,
              "end": 76
            },
            {
              "start": 90,
              "end": 90
            },
            {
              "start": 97,
              "end": 97
            },
            {
              "start": 130,
              "end": 131
            },
            {
              "start": 133,
              "end": 133
            },
            {
              "start": 134,
              "end": 135
            },
            {
              "start": 137,
              "end": 137
            }
          ],
          "detail": "76-76,90-90,97-97,130-131,133-133,134-135,137-137"
        },
        {
          "path": "migrations/postgres/000026_project_wallet_balance_snapshots.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 39,
              "end": 40
            },
            {
              "start": 41,
              "end": 43
            },
            {
              "start": 50,
              "end": 115
            }
          ],
          "detail": "39-40,41-43,50-115"
        },
        {
          "path": "migrations/postgres/000029_route_affinities.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 25,
              "end": 27
            }
          ],
          "detail": "25-27"
        },
        {
          "path": "migrations/postgres/000031_pricing_change_audits.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 37,
              "end": 46
            }
          ],
          "detail": "37-46"
        },
        {
          "path": "migrations/postgres/000032_change_sets.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 93,
              "end": 107
            }
          ],
          "detail": "93-107"
        },
        {
          "path": "migrations/postgres/000033_credit_redemptions.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 10,
              "end": 11
            },
            {
              "start": 105,
              "end": 107
            },
            {
              "start": 110,
              "end": 113
            },
            {
              "start": 114,
              "end": 127
            }
          ],
          "detail": "10-11,105-107,110-113,114-127"
        },
        {
          "path": "migrations/postgres/000034_credit_redemption_limits.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 8,
              "end": 20
            }
          ],
          "detail": "8-20"
        },
        {
          "path": "migrations/postgres/000035_api_key_concurrency_leases.sql",
          "kind": "parse_partial",
          "ranges": [
            {
              "start": 6,
              "end": 8
            }
          ],
          "detail": "6-8"
        }
      ],
      "status": "known_gaps"
    },
    {
      "requested_scope": "tests/",
      "scope": "tests",
      "total": 1,
      "returned": 1,
      "truncated": false,
      "entries": [
        {
          "path": "tests/go_test_mapping.md",
          "kind": "not_indexed_file",
          "detail": "gitignore"
        }
      ],
      "status": "known_gaps"
    },
    {
      "requested_scope": ".github/",
      "scope": ".github",
      "total": 0,
      "returned": 0,
      "truncated": false,
      "entries": [],
      "status": "no_recorded_issue"
    }
  ],
  "scope_total": 28,
  "scope_returned": 28,
  "scope_truncated": false,
  "has_more": false,
  "caveat": "Best-effort signal only. No recorded issue does not prove graph or source completeness; read flagged source and qualify claims when metadata is changed or unavailable."
}
```

变更源码、排除策略、工具或配置后重新核对相关范围；严格只读角色不各自刷新共享图。图谱定位优先、有界源码补证形成明确限制，检查成功不替代业务测试。


<a id="source-graph-limits"></a>
## 历史输入集与图File的完整限界

一条coverage请求包含6个scope，共28条缺口记录（11parse_partial）；它不是全仓完整性证明。1357源码输入与1344个File节点交集1315：42源码输入没有File节点，反向29节点均为Markdown且不在source freshness口径。`.cargo/audit.toml`与`.cargo/config.toml`在源码哈希中、缺File，且不属该6scope。完整差集如下，不把图空结果当文件不存在。

图内29Markdown的当前内容/片段不能由1357源码SHA不变担保；本包13产品文档各有独立冻结内容SHA，仍不能推及所有29MD图节点。对Markdown问题直接读取对应当前文件并记录内容SHA；对缺File的配置/资产，用准确路径有界读取与source-manifest哈希核对，不要求本次刷新/重建图。动态traits/macros同样需有界源补证。旧VERIFY_INDEX PASS仅覆盖源绑定、图身份、所查覆盖排除与片段调用四项，不扩展为全文档或业务通过。

### 全部42个输入缺File节点

| 当前源码输入（均在1357哈希口径） | 补证方式 |
| --- | --- |
| [.cargo/audit.toml](../../.cargo/audit.toml) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [.cargo/config.toml](../../.cargo/config.toml) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [.cbmignore](../../.cbmignore) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [.dockerignore](../../.dockerignore) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [.gitignore](../../.gitignore) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [.node-version](../../.node-version) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [Cargo.lock](../../Cargo.lock) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [LICENSE](../../LICENSE) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [LICENSES/LGPL-3.0-only.txt](../../LICENSES/LGPL-3.0-only.txt) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [LICENSES/POSTGRESQL_WINDOWS_THIRD_PARTY_LICENSES.txt](../../LICENSES/POSTGRESQL_WINDOWS_THIRD_PARTY_LICENSES.txt) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [LICENSES/PostgreSQL.txt](../../LICENSES/PostgreSQL.txt) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [NOTICE](../../NOTICE) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-http/assets/favicon.ico](../../crates/conduit-http/assets/favicon.ico) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/anthropic/anthropic-error.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/anthropic-error.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/anthropic/anthropic-stop.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/anthropic-stop.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/anthropic/anthropic-think.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/anthropic-think.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/anthropic/anthropic-tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/anthropic-tool.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/anthropic/llm-stop.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/llm-stop.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/anthropic/llm-think.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/llm-think.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/anthropic/llm-tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/anthropic/llm-tool.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/openai/deepseek-reasoninig.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/deepseek-reasoninig.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_2.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_2.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_3.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-multiple_choice_tool_3.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/openai/openai-parallel_multiple_tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-parallel_multiple_tool.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/openai/openai-stop.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-stop.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/openai/openai-tool.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-tool.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [crates/conduit-transformers/tests/fixtures/openai/openai-tool_2.stream.jsonl](../../crates/conduit-transformers/tests/fixtures/openai/openai-tool_2.stream.jsonl) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/.env.example](../../frontend/.env.example) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/.gitignore](../../frontend/.gitignore) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/.npmrc](../../frontend/.npmrc) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/.prettierignore](../../frontend/.prettierignore) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/.prettierrc](../../frontend/.prettierrc) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/NOTICE](../../frontend/NOTICE) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/package.json](../../frontend/package.json) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/public/favicon.ico](../../frontend/public/favicon.ico) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/public/logo.svg](../../frontend/public/logo.svg) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/src/assets/vite.svg](../../frontend/src/assets/vite.svg) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/src/locales/en/settings.json](../../frontend/src/locales/en/settings.json) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/src/locales/zh-CN/settings.json](../../frontend/src/locales/zh-CN/settings.json) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [frontend/tsconfig.json](../../frontend/tsconfig.json) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |
| [scripts/licenses/rust-third-party.hbs](../../scripts/licenses/rust-third-party.hbs) | 按准确路径读取；内容SHA见[source-manifest](source-manifest.md)，不使用图空结果断言缺实现 |

### 全部29个File节点不在源码freshness输入集

| 图File路径 | freshness边界 |
| --- | --- |
| `.github/pull_request_template.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `AGENTS.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `CODE_OF_CONDUCT.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `CONTRIBUTING.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `LICENSING.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `README.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `README_CN.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `RELEASE_GATES.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `RELINKING.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `ROADMAP.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `SECURITY.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/domain-model.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/frontend-graphql-operation-inventory.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/http-route-inventory.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/internal-admin-api.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/knowledge/index.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/knowledge/indexing.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/postgres-database-scope.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/postgres-performance-baseline.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/production-deployment.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `docs/project-access-unification.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `frontend/src/locales/README.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `migrations/INVENTORY.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `migrations/postgres/README.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `scripts/e2e/README.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `scripts/provider-compatibility/README.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `tests/contracts/README.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `tests/contracts/llm_cases/README.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
| `tests/contracts/routes_snapshot.md` | Markdown；1357源码SHA不担保当前内容，必要时有界读原文件另核SHA |
