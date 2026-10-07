# Conduit API 修复 TODO

更新时间：2026-10-07（Europe/London）。唯一源码项目：G:/fubox_API。

本文件是本轮唯一的人类进度入口；机器任务状态以本项目 `.codex-workflow/ledger.sqlite3` 的 `quality-repair-20261006` 为准。Root /root 更新本文件；实现者回报证据，不另建平行TODO。

## 目标与约束

- 完整修复21项已确认独立根因（9P1/12P2），沿用原始问题ID及成功标准。
- 唯一共享实现者：/root/index_docs；唯一独立验证者：/root/backend_research，已完成a1审查、a2必要返工增量及a3索引文档增量。验证子Agent上限1，不增加审查/验收/交付验证者。
- 稳定候选集中独立判断一次；复用有效检查，失败或候选变化只补必要检查。不要把原命令失败或跳过改标通过。
- 保留既有修改；PostgreSQL测试隔离、默认无真实provider凭据。同步公共fixtures、配置、权威docs/操作步骤/Mermaid与索引。
- 2026-10-07用户明确授权将当前修复提交并push到既有云端仓库；仅此Git交付获授权，不部署、不操作生产DB或真实provider。

## 当前恢复摘要

run=quality-repair-20261006，requirements_rev=repair-r1，ledger revision=93（5项报告接受、2项交付任务因空间阻塞；运行任务0）。21项源码及适用门禁已获唯一验证者APPROVE：a1通过17项，a2只核UX-04/F-02/F-06/F-12及必要共享依赖，四项通过。没有第二验证者或全21重复验收。整体仍UNMET_INDEX，不能宣称全局完成。

当前a3冻结candidate content:3696ee9ff0d24de64db6cd9e48b2e5b6493172817333b450e1d46c4e73fd7211（源码与a2不变，仅三份knowledge文档变化）；1366源码SHA4c2a73a17e7cd18da0300d75c171e7dbb598eae33a6592e2e98793e79eeca3f0。唯一验证者实际核1380内容/1366源码/120证据SHA、8源码/16内容delta及21项来源，全部匹配。a1/a2不可变报告保留。

作者实际workspace6259通过/0失败/9既有忽略（28targets）；metadata/fmt/workspace Clippy退出0。Clippy有警告：bin test target73条（70重复），workspace去重总量未统计；作者零警告摘要失准，原日志及独立澄清保留。前端四门禁通过、117unit通过/0fail/0skip，lint71既有警告；55 Mermaid解析证据复用，当前3031本地链接通过。验证者独立读真实代码/回归/日志，没有另跑作者构建。

a2所属进程全部退出，验证者实际确认PG38896/daemon44456及55436监听不存在；全部执行会话终态，原10用户PG保留。两处自动审核拒绝清理未执行、未重试；缓存、停止数据库目录、备份/证据保留。

索引：旧G三文件与C备份六SHA一致，STALE。v6原生ACL检查在pipeline前失败；v7修复本任务C目录ACL后pipeline generic失败，两个effects均failed，G未变、无需恢复。C缓存新generation/full coverage不是权威发布。只读研究强推断失败在cache发布后artifact export，具体stage/errno UNKNOWN；worker cleanexit且无profile会删除详细日志，不认定空间/源码原因。

a3仅索引单次诊断已失败并停止，native-index-a3-v8=failed；保留worker日志实际定位artifact.export stage=write_artifact / err=write_temp / errno=28，G临时写入空间不足。旧G三SHA未变，无恢复必要；.tmp/args/reserve均无，所属daemon37184/worker43976已退出，未自动重试。不能追溯把v7 UNKNOWN改成ENOSPC，实际新压缩大小未输出。

剩余外部条件：用户腾出G空间并回复真实可用空间；新异步状态问题pending。128MiB仍只是先前保守预算，不保证充分。writer已完成必要knowledge状态与a3冻结报告，所有执行工具静止；源码4c2/21APPROVE/6259与117有效结论复用。同一backend已核索引失败证据及三份文档增量APPROVE；无第二验证者，无重跑源码门禁。IMPLEMENT_ALL与FINAL_EVIDENCE因真实空间阻塞登记blocked；运行任务/所属后台/pending effects均0，交接已保存，整体仍未完成。

## 统一条目

| ID | 级别 | 修复目标 | 可观察成功标准 | 状态 | 责任 | 当前证据 |
|---|---|---|---|---|---|---|
| ACCESS-01 | P1 | 平台Owner授予与项目Owner豁免分开 | 非平台Owner+平台write_users+项目Owner不能授平台Owner；平台Owner正常授予/项目成员Owner操作保留 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| UX-01 | P2 | 记住我真实控制临时/持久会话 | 未勾选仅临时会话，勾选跨浏览器持久；登出/过期清理两种存储 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| UX-02 | P2 | 移除虚假密码重置承诺，提供准确状态和有效处理指引 | 页面不再假发送；呈现真实可用状态/处理方式 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| UX-03 | P2 | 请求详情返回有效路由及上下文 | 返回真实来源或已注册列表，权限/项目上下文适用 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| UX-04 | P2 | 用户保存领域错误分类/字段反馈，避免重复通用toast | 重复邮箱字段反馈、权限拒绝可行动提示，同一失败只一处通知 | 唯一独立核定APPROVE（a2）；整体索引待 | index_docs | repair-review-a2-result.json逐ID；真实调用链回归/原日志已独立核验 |
| F-01 | P1 | live全生命周期deadline与准入租约/预留一致 | 开始响应后的等待也能到期取消、finalize，并发lease/钱包reserve不早于活动期释放 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-02 | P2 | request/execution取消状态一致 | 取消后父子均保留取消原因，正常失败与成功不混淆 | 唯一独立核定APPROVE（a2）；整体索引待 | index_docs | repair-review-a2-result.json逐ID；真实调用链回归/原日志已独立核验 |
| F-03 | P1 | 成功usage初次写失败有持久/幂等可恢复计量依据 | 初次PGusage写失败仍有可重放权威输入，恢复幂等，不重复收费或静默丢失 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-04 | P1 | live遵守body/chunks存储策略 | 所有成功live写入点遵守body/chunks各自关闭组合 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-05 | P2 | 流累计历史总量预算 | 累计事件/字节有总量预算，超过后有明确终止/资源释放 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-06 | P2 | 自动备份保留天数真实执行 | 正数保留天数清理本应用过期备份，0永久；失败可重试 | 唯一独立核定APPROVE（a2）；整体索引待 | index_docs | repair-review-a2-result.json逐ID；真实调用链回归/原日志已独立核验 |
| F-07 | P2 | 已提交与缓存修复结果准确反馈 | DB提交成功与缓存修复失败明确区分，UI不伪装未提交 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-08 | P1 | 配置原子更新/CAS，任务状态不覆盖旧JSON | 并发局部更新不丢失，后台状态写回不覆盖管理员未来配置 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-09 | P1 | 缓存回填版本保护，阻止旧值跨失效回填 | 暂停旧读跨新提交/失效后不能回填旧缓存；Memory/Redis/TwoLevel语义一致 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-10 | P1 | 外部工件GC及hydration删除生命周期闭合 | 外部对象清理可恢复，已过期内容不能hydrate重现，删除记录不遗失待删引用 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-11 | P1 | 视频归档公平扫描/可持久重试，防首批饥饿 | 首批坏/缺URL视频不阻塞后续有效记录，持久退避/重试可恢复 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-12 | P2 | 多实例副作用任务持久认领/幂等 | 两个实例相同due任务由持久claim认领；崩溃claim可过期重试，副作用幂等 | 唯一独立核定APPROVE（a2）；整体索引待 | index_docs | repair-review-a2-result.json逐ID；真实调用链回归/原日志已独立核验 |
| F-13 | P2 | 后台句柄监督、取消与有界关停 | 实际job句柄受监督，停止新任务/取消/有界等待，各自资源退出 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-14 | P2 | 陈旧判定用实际活动/lease，不误判正常长流 | 正常长流不会因60秒无终结写入被当stale；失活任务仍可清理 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-15 | P2 | 超时取消路径metrics减计数/记录最终504 | 正常/错误/超时取消inflight均归零，最终504计数一次 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |
| F-16 | P1 | 备份各section同一致快照，验证可恢复引用 | 所有导出section使用同一PG快照，并发价格变更下归档引用完整、支持范围可恢复 | 唯一独立核定APPROVE（a1）；17未变结论复用，整体索引待 | index_docs | repair-review-a1-result.json逐ID；原source/证据SHA已独立匹配 |

## Git 交付约定

2026-10-07用户明确授权当前修复提交并push。目标为既有GitHub仓库404F0X/Conduit-API的origin/main，采用普通推送。范围为已批准源码、知识文档和本TODO；保留原有AGENTS/README及未授权文件，运行账本、缓存、凭据和原始日志不随源码提交。代码验收结果复用，不新增验证者或重复门禁。

本文件及知识文档内指向.codex-workflow的链接是当前工作区本机审计证据，云端新checkout不包含这些运行附件；用户操作流程、项目知识及执行反思位于docs/knowledge。实际提交/推送SHA及交付结果登记在本机唯一账本和交付报告，避免在被提交文档中嵌入自身commitSHA。权威图STALE/ENOSPC待办继续保留。

## 剩余工作

- [x] 21项源码及必要生产调用方/回归、配置/fixture/docs修复；唯一验证者a1通过17项、a2通过4项，不全21重复验收。
- [x] workspace6259/0/9、frontend117/0/0、适用门禁、55 Mermaid及当前3031links，原FAIL/忽略保留。
- [x] a2最终1380内容/1366源码/120证据manifest冻结并独立匹配，所有a2所属资源实际退出。
- [x] a3单次profile诊断保留日志，已定位本次G临时写入ENOSPC，原G/备份完好、所属进程退出；没有自动重试。
- [ ] 外部条件：用户腾出G空间后明确回报；未回复。权威索引发布及identity/freshness/coverage仍待。
- [x] 同一backend核a3失败证据/三知识文档deltaAPPROVE，复用21源码/工程结论；无第二验证者。
- [ ] 用户实际腾出G空间后显式重规划仅索引发布；原两次返工计数保留，不自动第三返工或换ID重置。新权威索引成功后仍仅同一验证者核索引增量。
- [x] 本轮执行反思已记录调度、工作流/接口错误、上下文管理和索引诊断经验，并明确整体尚未完成。
- [ ] 权威索引实际发布后接受全局交付、更新最终完成状态；当前不finish/不伪称完整交付。

## 覆盖限制与授权记录

Docker CLI有但daemon不可达，实际container/volume为NOT_RUN；compose独占journal卷/env与Dockerfile非root目录权限已同步，权威说明有既有卷/bind owner检查步骤。此限制应保留到最终报告。

自动审核拒绝删除本任务失败rustc incremental工作目录，报告原样理由为`CreateProcess Rejected(... rejected: blocked by policy)`；详细policy原因未知，未执行，target保留。该G删除动作已停止，不换工具/壳/执行者或伪装move重试。另C:/ConduitRepair-quality-repair-20261006/target（3,520,948,656B）经owned/reparse预检后删除也被CreateProcess审核拒绝blocked by policy，未执行；C动作同样停止，缓存保留。用户空间状态询问源于真实G预算缺口，时间流逝不算回复/许可。

当前合同与必要scope追加记录在本run artifacts：implementation合同；schema-config-scope-v1（migrations/postgres、根配置examples、.cbmignore）；journal-deployment-scope-v2（compose.yml/必要Dockerfile）；isolated-build-runtime-scope-v3（C专属临时资源）。旧ledger paths不可原地扩展，最终具体新增根文件artifacts+完整SHA manifest+唯一验证者覆盖，不能假称旧paths自动覆盖。

F03采用有限持久journal＋PG receipt事务幂等交接现有结算；F16采用整次dump同PG只读快照。关键设计与恢复依据见recovery-contracts.md，最终新状态以真实role结果为准，不用历史pending/NOT_RUN覆盖当前证据。

## 证据入口

- [权威项目知识入口](docs/knowledge/index.md)
- [当前需求](.codex-workflow/runs/quality-repair-20261006/artifacts/requirements-repair-r1.md)
- [Root恢复入口](.codex-workflow/runs/quality-repair-20261006/artifacts/root-checkpoint.md)
- [冻结作者结果](.codex-workflow/runs/quality-repair-20261006/artifacts/implementation-a3-result.json)
- [完整SHA候选清单](.codex-workflow/runs/quality-repair-20261006/artifacts/implementation-a3-candidate.json)
- [唯一独立审查a1实际结果](.codex-workflow/runs/quality-repair-20261006/artifacts/repair-review-a1-result.json)
- [唯一独立索引文档增量a3](.codex-workflow/runs/quality-repair-20261006/artifacts/repair-review-a3-result.json)
- [唯一独立增量审查a2](.codex-workflow/runs/quality-repair-20261006/artifacts/repair-review-a2-result.json)
- [a3索引增量合同](.codex-workflow/runs/quality-repair-20261006/artifacts/contract-implement-all-a3-index-only.json)
- [索引失败诊断](.codex-workflow/runs/quality-repair-20261006/artifacts/index-failure-tracing.md)
- [集中返工a2合同](.codex-workflow/runs/quality-repair-20261006/artifacts/contract-implement-all-a2.json)
- [固定版本索引实现事实](.codex-workflow/runs/quality-repair-20261006/artifacts/index-persistence.md)
- [当前阶段执行反思](.codex-workflow/runs/quality-repair-20261006/artifacts/repair-execution-reflection.md)
- [原16项链路诊断](.codex-workflow/runs/project-quality-chains-20261005/artifacts/quality-report.md)
- [权限与UX诊断](.codex-workflow/runs/permission-ux-20261006/artifacts/auth-ux-report.md)

历史细节保留在角色报告和日志，不在本文件堆积进度历史。范围为确认缺陷；既有能力边界、未证风险保留为后续需求材料。
