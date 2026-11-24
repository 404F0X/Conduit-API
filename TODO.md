# Conduit API 修复 TODO

更新时间：2026-10-09（Europe/London）。唯一源码项目：G:/fubox_API。

本文件是唯一人类进度入口；机器状态以本项目 .codex-workflow/ledger.sqlite3 最新运行lite-full-preview-20261009为准（done10，预览服务按用户要求持续运行；实现run已done74）；runtime-replacement-20261008已完成、quality-repair-20261006保留历史。Root /root维护，不建立平行TODO。旧运行revision268、assignment24/24并已cancelled（非finish）；草稿/渠道修复已accepted，历史4blocked保留。runtime-replacement-20261008已完成（revision12，assignment1/24，RUNTIME_DELIVER_8090 a1 accepted）；仅执行用户新授权的包替换。

## 当前可用：Lite／完整版本机测试服务

run lite-full-preview-20261009 / preview-r1 已done revision10，TEST_DELIVER a1 accepted9，assignment1；用户2026-10-09授权服务持续运行供手工测试。打开 **http://127.0.0.1:8091/**，首次初始化管理员姓名、邮箱、自定密码／确认及品牌，再用新账户登录。使用独立新测试库，无预置账号；原8090、NEWAPI3000、用户PG5432保留。

- [x] PREVIEW-START：4START全部pass。1404源码／10文档／1012dist与8410137f/sourcec0b3065e一致，必要locked构建exit0、源码修改0；真实agent-browser初始化页面85本地请求200／JS错误0；真实DB身份先核再应用35迁移至000038/public。
- [x] PREVIEW-HANDOFF：交付最终观测API22928／127.0.0.1:8091、独立PG32972／127.0.0.1:55439及必要children持续运行，精确启停脚本已保存；PREVIEW_START与PREVIEW_BUILD_CACHE_CLEANUP均succeeded、pending0。首次清理本run新C构建缓存2092106539B（约2.1GB），保留runtime、PG测试数据、日志和交付证据。

仅1个交付叶子、0新增验收Agent，复用原唯一verifier11/11及有效源码门禁，没有重复全功能验收。此新库初始化、登录及配置由用户手工体验；匿名GraphQL401只证明真实JWT门禁，不声称此新库认证业务调用已执行。未调用真实供应商／支付、未访问业务库、未Git发布。既有图谱和额外tsc覆盖限制沿用；pg_ctl数值exit未知、实际启动及身份已确认，无重复启动。停止脚本语法已查，未执行以保留活服务。

交付入口：[交付报告](.codex-workflow/runs/lite-full-preview-20261009/artifacts/preview/report.md)、[资源记录](.codex-workflow/runs/lite-full-preview-20261009/artifacts/preview/resources.json)、[启动脚本](.codex-workflow/runs/lite-full-preview-20261009/artifacts/preview/start-preview.ps1)、[停止脚本](.codex-workflow/runs/lite-full-preview-20261009/artifacts/preview/stop-preview.ps1)、[本轮反思](.codex-workflow/runs/lite-full-preview-20261009/artifacts/execution-reflection.md)、[Root恢复点](.codex-workflow/runs/lite-full-preview-20261009/artifacts/root-checkpoint.md)。服务PID为职责叶子交付时观测，不代替后续启停脚本的实时身份核对。

## 已完成本轮：Lite默认开关与完整版治理

run lite-full-implementation-20261008，requirements lite-full-r1；唯一ledger已done revision74，所有四任务accepted。用户授权规划及代码落地，当前源码候选8410137f／sourcec0b3065e。此轮成果为源码与文档，8090运行包未在此轮更新，Git发布也未执行。

- [x] MODE-ARCH：当前基线678d2f／1385已绑定，计划67b961明确同功能集、独立模式／执行／权限／审批、完整治理与Project账务契约，accepted7。
- [x] MODE-CODE：a3源码核对approve并accepted64；R1真实retry身份、R2组合定价权限、R3真实PriceTier／AccessPlan版本依赖闭合。119累计目标变更、1404源码输入、143相关dirty/new文件，保留其他修改。
- [x] MODE-ACCEPT：仅一个验收Agent /root/lite_full_verify a1，十项成功标准全部pass、accepted72；新增真实JWT／生产GraphQL HTTP／隔离PG／本机假上游9项及实际published route条目冲突2项，合计11/11。复用有效门禁，没有重跑全套验收。
- [x] MODE-REFLECT：Root完成任务后的调度、工作流、上下文、资源与两次返工反思已定稿；检查点与证据指针已保存。

实现保持同一代码／功能集：Lite管理员6个／用户5个日常入口及高级目录；Full完整呈现治理。持久三态覆盖、模块API／worker启停、角色授权及审批独立。111固定typed actions／161根mutation分类／10治理kind；原生表单、私有命令、独立审核／发布、同事务ApproveApply双职责与双事件。Project为真实可用账本，本人／所选模型权益如实呈现，累计成员Credits预算跨所有key；模式切换保留显式选择、旧包和历史，核心账务和已发权益继续。

最终门禁：Rust workspace6292通过／0失败／9ignored，metadata／fmt／clippy通过（警告保留）；未变前端四门禁、132unit和19模拟浏览器证据复用，非新执行。源码核对及唯一验收独立匹配最终候选和日志绑定；未把缺DSN提前返回当作SQL通过，111目录分类不等于111项独立PG测试。

权威索引：正确G:/fubox_API / conduit-api-main持久图代次51921nodes／229362edges，1404输入的source／generation匹配，修复文件无File缺口。109metadata_changed／14partial／42missing File／39extra File诊断仍未解决，不能宣称全图完整。额外tsc80条真实诊断，整体基线归属UNKNOWN；未独立跑全部worker调度周期、真实provider或支付。唯一验收明确这些为本轮非阻断覆盖限制，完整说明在报告。

资源：自有API／PG及子进程已退出，18469／55438释放、pending0；首次安全清理自有C构建缓存4891578362B和假PG数据1219767867B，共6111346229B（约6.1GB）。历史被拒G缓存未动。本任务零字节usage-journal首次删除在CreateProcess前被策略拒绝，理由blocked by policy、具体原因UNKNOWN，无进程／无删除／未重试，保留残留与原拒绝证据。

证据与使用入口：[验收报告](.codex-workflow/runs/lite-full-implementation-20261008/artifacts/verification/report.md)、[冻结作者结果](.codex-workflow/runs/lite-full-implementation-20261008/artifacts/implementation-a3/result.json)、[独立源码核对](.codex-workflow/runs/lite-full-implementation-20261008/artifacts/source-review-a3/report.md)、[架构计划](.codex-workflow/runs/lite-full-implementation-20261008/artifacts/architecture/plan.md)、[权威知识入口](docs/knowledge/index.md)、[用户步骤与流程](docs/knowledge/operations.md)、[客户端流程](docs/knowledge/frontend-client-operations.md)、[执行反思](.codex-workflow/runs/lite-full-implementation-20261008/artifacts/execution-reflection.md)、[Root恢复点](.codex-workflow/runs/lite-full-implementation-20261008/artifacts/root-checkpoint.md)。

历史说明：下列章节状态仅属于相应旧run，保留当时失败／阻塞／取消事实；旧STALE／ENOSPC与方案“尚未实现”文字不代表本轮当前状态。历史quality-repair的返工计数与cancelled状态不重置，其全局finish未由本轮冒充；当前索引事实以上述新报告为准。

## 已完成梳理：追踪 NEW API，确定 Lite 默认功能

run newapi-lite-baseline-20261008 已恢复并完成（revision20 / done）。保留原任务a1、需求与预算；正式官方基线补齐，旧QUESTION草稿原样归档后一次完成最终对照。Lite与完整版保留同一完整功能集，本轮确定默认入口和开关方案，没有实施或发布。

- [x] NEWAPI-BASELINE：RC42/6370固定，24域官方功能与admin/user链、默认/可选条件已保存；查询时HEAD差异另记，静态研究不等于运行验收。
- [x] LITE-RETAIN：22行覆盖24官方域，默认保留/高级收起/按需启用及P0/P1；6个管理员/5个用户入口、7/6步操作、3Mermaid、8后续条件已完成文档。
- [x] LITE-P0：核心实施选择已在当前MODE-ARCH就绪契约中明确：沿用Project账本、本人/模型权益、跨密钥成员预算、统一治理及独立开关。具体行为通过与兼容能力承诺仍依赖候选证据。
- [ ] LITE-IMPLEMENT：并入当前MODE-CODE，已经开工，完成状态只在本文件顶部维护；方案不等于上线。支付、其他登录、推广签到、媒体与内置试用按实际选择核实，不自动扩大开发。

本轮文档：[NEW API官方基线](.codex-workflow/runs/newapi-lite-baseline-20261008/artifacts/newapi-baseline/inventory.md)、[Lite默认功能与用户流程](.codex-workflow/runs/newapi-lite-baseline-20261008/artifacts/lite-retain/lite-feature-baseline.md)、[执行反思](.codex-workflow/runs/newapi-lite-baseline-20261008/artifacts/execution-reflection.md)。旧草稿/QUESTION保留于lite-retain/interrupted-draft；2恢复职责顺序执行、0新增verify，无重复工程验收；源/8090/DB/Git未改。新职责资源0/pending0，旧actor无PID证据的资源状态UNKNOWN，不停止用户进程。权威图STALE与原索引发布待办仍保留。

## 已完成方案：Lite／完整版与可选审批

run product-modes-review-20261008 已完成（revision13，2原生角色/无verify）；用户r2澄清同功能集，仅默认开关/入口减负：Lite类似NEWAPI简洁、Full完整呈现可选审批/RBAC。方案完成，源码尚未改造；8090未在此轮更新。

- [x] MODE-FACTS：只读事实已接受；SIMPLE/ENTERPRISE已有且共享权限，缺模块开关；审批仅3kind并有映射/分组价格/角色/项目资金等直写入口。人工额度参与运营风险，与自动quota控制链分离；NEWAPI固定官方来源已核。
- [x] MODE-PROPOSAL：冻结b371af5e方案；同功能集/默认与显式设置分离、功能矩阵、3Mermaid用户流程、12后续成功标准，现状/未来能力分开。
- [x] MODE-DECISIONS：同代码同安装包/SIMPLE-ENTERPRISE作为推荐；审批独立可配，全同类写入口治理，单管理员显式自审/第二审核人初始化方案；具体审批支持类型/紧急恢复规则列为后续技术设计选择。
- [ ] MODE-IMPLEMENT：并入顶部当前MODE-CODE；两组默认、持久覆盖、真实执行门禁及完整可选治理正在实现，不能将已完成的提案视为上线。

本轮文档：[产品方案](.codex-workflow/runs/product-modes-review-20261008/artifacts/mode-product/product-proposal.md)、[当前代码事实](.codex-workflow/runs/product-modes-review-20261008/artifacts/mode-facts/report.md)、[执行反思](.codex-workflow/runs/product-modes-review-20261008/artifacts/execution-reflection.md)。事实只读，未运行quota/provider或工程验收；原权威图STALE/索引发布与旧质量待办保留。

## 已完成：渠道页交互及103个价格应用卡死

run price-apply-freeze-20261008 已完成（revision16）；当前运行版本0.1.0-alpha.10+ui.587e879a11f6，用户新增数据保留。

- [x] PRICE-FREEZE：已证实103价格重复挂载阻塞和快速关闭重开编辑器的旧timer清理故障；4局部文件修复，587e候选冻结，价格/API/schema语义不变。
- [x] PRICE-FLOW：4前端门禁/130单测、6真实browser组及追加重复apply保留断言PASS；首中末滚动重挂、CNY/档位/缓存及完整103行mock草稿payload正确。作者针对性检查，未新增独立verify。
- [x] PRICE-RUNTIME：587e包已恢复8090（API49188/原PG6140），19public/static含12served assets SHA PASS；独立old885d备份22文件/214023096B、logical full parse PASS。同schema原PG和新增实时数据保留，4交付AC PASS/任务accepted/effect succeeded；run done revision16，未新增verify或重跑旧整套验收。

源码证据：[本轮作者报告](.codex-workflow/runs/price-apply-freeze-20261008/artifacts/price-ui/report.md)、[运行交付](.codex-workflow/runs/price-apply-freeze-20261008/artifacts/price-runtime-delivery/report.md)、[执行反思](.codex-workflow/runs/price-apply-freeze-20261008/artifacts/execution-reflection.md)。真实首次apply主线程长任务4786ms→61ms，心跳4981.5ms→78.7ms；自动化wall time另记，未声称所有操作/真实NEWAPI写入均覆盖。

## 已完成：事件包计数文案与底部裁切

机器当前协调run event-readable-ui-20261008已完成（同一ledger，revision14/status done）；09ed已交付有效baseline保留。局部LIGHT源码实现不新增独立验证者，保留前端四门禁与定向真实浏览器检查。

- [x] UI-COUNT：将+ / Δ / − / !计数换成直观新增/修改/删除/需处理文字，明确项数，适用于桌面和手机。
- [x] UI-SCROLL：长列表可滚到完整最后一行，待处理徽章/名称不被panel与分页裁切；检查小窗口/缩放。
  最终包含200%文字仿真下自然高度页头，2路由×8布局边界及键盘入口；没有声称原生浏览器菜单缩放已测。
- [x] UI-RUNTIME：885d源/3538前端已实际更新8090，API56164版本0.1.0-alpha.10+ui.885d503becb5持续运行；18public/static与12新资源SHA通过，原PG6140/schema000037/live数据保留。4交付AC PASS、正式任务accepted、effect succeeded。

本轮证据：[UI报告](.codex-workflow/runs/event-readable-ui-20261008/artifacts/ui-implementation/report.md)、[运行交付](.codex-workflow/runs/event-readable-ui-20261008/artifacts/ui-runtime-delivery/report.md)、[执行反思](.codex-workflow/runs/event-readable-ui-20261008/artifacts/execution-reflection.md)。新独立备份桌面conduit-api-ui-backup-20261008-readable-r1，22文件/213980038bytes，logical snapshot可完整解析；同schema回滚保持live DB/journal，未实际演练。源LIGHT仅1实现者、无新增verify/完整旧验收；原有功能分布和权威索引待办保留。

## 历史已完成交付：替换本机8090运行包

用户2026-10-08明确授权替换正在运行的8090，保留数据与配置，供用户手工测试。机器当前状态改由同一ledger内runtime-replacement-20261008管理；旧quality-repair-20261006仅停止调度（cancelled，非全局finish），已批准源码与4个历史blocked、旧返工记录均保留。

- [x] RUNTIME-8090：09ed全1384源码与A2前端1011文件一致；release-binary构建exit0，版本0.1.0-alpha.10+runtime.09ed8b147a13。
- [x] RUNTIME-BACKUP：Desktop/conduit-api-runtime-backup-20261008-09ed一致备份1850文件/284220317bytes，logical归档可解码、停机PG数据hash一致；回滚需相配旧包与数据库。
- [x] RUNTIME-REPLACE：新8090 PID21588/ownedPG6140持续运行；18项public/static检查和12个最新资产SHA通过，000037已应用；83旧表保留，108部署行仅启动updated_at更新。workflow_deliver同合同a1交付4AC PASS，已接受，无新增verify或重复源码验收。
- [ ] UX-DISTRIBUTION：用户反馈功能分布乱；新包试用后收集具体入口/操作链路，再决定收拢方案。本次未扩大源码修改。

交付证据：[报告](.codex-workflow/runs/runtime-replacement-20261008/artifacts/runtime-delivery/report.md)、[结果](.codex-workflow/runs/runtime-replacement-20261008/artifacts/runtime-delivery/result.json)、[运行回滚脚本](.codex-workflow/runs/runtime-replacement-20261008/artifacts/runtime-delivery/rollback.ps1)、[执行反思](.codex-workflow/runs/runtime-replacement-20261008/artifacts/runtime-execution-reflection.md)。备份在桌面conduit-api-runtime-backup-20261008-09ed。用户业务/真实provider测试待用户；实际恢复演练未运行，不承诺binary-only回滚。临时runtime-delivery/__pycache__自动清理被CreateProcess blocked by policy拒绝、未执行且不重试；缓存保留。无源码或Git更改。

## 历史已完成：草稿事件包与 NEWAPI 渠道流程

本轮需求 [draft-package-r3](.codex-workflow/runs/quality-repair-20261006/artifacts/requirements-draft-package-r3.md) 的本地源码与文档范围已获唯一实际验证者批准。一次操作形成事件包，摘要列表与按需详情分开；NEWAPI一个站点地址，创建即发现、显示阶段结果，支持失败阶段重试和显性入口。未替换用户8090运行包，未对本轮新增Git提交、push、标签或部署。

| ID | 成功标准 | 最终状态 | 当前证据 |
|---|---|---|---|
| DRAFT-01 | 独立事件包、完整校验、所有权/版本/幂等和原子应用 | 完成 | 24AC中有效A1语义与Rust/PG证据复用 |
| DRAFT-02 | 摘要/按需详情、完整差异/错误/活动，终态恢复后不再提供旧动作 | 完成 | A2 F01/F03/F04/F06/F07；当前独立结论 |
| PRICE-01 | 正确缓存默认/单位/采购换算和来源不确定性，完整支持的价格/档位/缓存变体差异 | 完成 | 固定上游契约+未变后端证据；A2 F03 |
| CHANNEL-01 | NEWAPI单地址、创建发现、保存后立即显示阶段与持久回执，部分成功/失败重试清楚 | 完成 | A1 AC17/19/20等复用；A2 F05/AC18 |
| CHANNEL-02 | 桌面及窄屏直接使用发现、结果和事件包入口，权限与键盘一致 | 完成 | A2 F02/AC16/AC21与真实390px回归 |
| VERIFY-R3 | 同一实际验证者集中检查，返工只核受影响行为与依赖 | 完成 | 24PASS：10当前增量判断+14有效A1复用；未重跑旧21 |
| DOC-R3 | 既有知识文档、用户步骤、Mermaid与任务后反思同步 | 完成（当前修复范围） | 5文档2418links0fail；17定义1实际解析+16复用；最终反思 |

最终源码 [作者a2结果](.codex-workflow/runs/quality-repair-20261006/artifacts/draft-implement-r3-a2-result.json) SHA70d2293d…，candidate09ed8b147a1312503d269fc8890254cc3f843c00de3c280643f66fd029d4178a；全62文件/增量15/1384源输入SHAf75a6a9d…。候选JSON60a4bd65…、报告8d54aa1d…。原A1 sourceef3/58文件/全部83作者产物保留，精确源码归档661444B，新文件均按a2版本保存。

唯一实际验证者/root/draft_verify_r3（原生workflow_verify）[a2最终结果](.codex-workflow/runs/quality-repair-20261006/artifacts/draft-verify-r3-a2-result.json) SHA8c9528d67a970ed5814930012ebeaadbf39f6646e05a3bf29296a50e5f5bed95、[报告](.codex-workflow/runs/quality-repair-20261006/artifacts/draft-verify-r3-a2-report.md) SHAad944c0b…：approve，F01–F07全部解决，24AC通过。a1集中14PASS/10FAIL与5实质+2轻微问题仍保留原报告，没有改写失败。原同一作者/root/index_docs负责一批返工，未增加第二验证者。Root仅核身份/版本/门禁并登记，不执行工程验收。

实际检查：frontend四门禁exit0，130unit/0fail/0skip，7真实React-Chromium隔离回归PASS，lint71warning。580后端输入/83A1产物及19GraphQL文档变量不变，A1metadata/fmt/fullworkspace6274PASS/9ignored、Clippy和真实PG/schema19有效复用。Clippy有警告、去重总量UNKNOWN，Vite大chunk提示保留；三零用例harness不当行为PASS。A2五相关文档2418本地链接无失败，17Mermaid定义中1实际改变解析、16复用；未变其他文档仍保留A1对应范围证据。

覆盖限制：A2额外tsc NOT_RUN；A1历史FAIL80诊断、跨依赖基线归因UNKNOWN，当前完整类型正确性不宣称通过。真实生产/provider、付费推理及完整无障碍/非默认features未运行。权威图仍STALE：1344graphfiles与1384sources差集70/30，旧三SHA未变、没有刷新；手工知识/source清单更新不代表图谱fresh。原索引发布待办仍保留，不能宣称全项目原目标完成。旧ENOSPC是历史，用户已手工清理、作者已确认可用空间；不继续沿用旧空间条件。

资源：作者全部自有服务/浏览器/工具会话退出；验证者a2未创建新资源。active owned/pending均0，18463/18464无监听。原PG44328与8子进程已停，55436无监听；唯一Gtarget最近实测约4.25GB保留以复用，未新增重复Rust缓存。用户服务、AGENTS/README/a.py/ops保持原样。原A1假认证state/config/namespace清理被自动审批CreateProcess Rejected blocked by policy拒绝，整个命令未启动、具体原因UNKNOWN；静态文件保留、无真实凭据，没有换工具/执行者重试。具体路径及原样证据在A1验收result background/cleanup-policy-rejection.json。

当前入口：[权威知识入口](docs/knowledge/index.md)、[用户操作和流程图](docs/knowledge/operations.md)、[前端操作说明](docs/knowledge/frontend.md)、[完成后执行反思](.codex-workflow/runs/quality-repair-20261006/artifacts/draft-package-execution-reflection.md)、[Root恢复检查点](.codex-workflow/runs/quality-repair-20261006/artifacts/root-checkpoint.md)。.codex-workflow链接为本机持久审计材料，不作为已发布到云端的附件。

此前PR41和alpha10标签已交付；本轮09ed增量未发布。后续若需发布或仅索引重规划，复用本轮有效结论，仅核实际变化，不重新全量验收，也不清零旧任务返工或budget。

## repair-r1 历史目标与约束

- 完整修复21项已确认独立根因（9P1/12P2），沿用原始问题ID及成功标准。
- 唯一共享实现者：/root/index_docs；唯一独立验证者：/root/backend_research，已完成a1审查、a2必要返工增量及a3索引文档增量。验证子Agent上限1，不增加审查/验收/交付验证者。
- 稳定候选集中独立判断一次；复用有效检查，失败或候选变化只补必要检查。不要把原命令失败或跳过改标通过。
- 保留既有修改；PostgreSQL测试隔离、默认无真实provider凭据。同步公共fixtures、配置、权威docs/操作步骤/Mermaid与索引。
- 2026-10-07用户明确授权将当前修复提交并push到既有云端仓库；仅此Git交付获授权，不部署、不操作生产DB或真实provider。

## repair-r1 冻结历史摘要（不代表当前状态）

run=quality-repair-20261006，requirements_rev=repair-r1，ledger revision=93（5项报告接受、2项交付任务因空间阻塞；运行任务0）。21项源码及适用门禁已获唯一验证者APPROVE：a1通过17项，a2只核UX-04/F-02/F-06/F-12及必要共享依赖，四项通过。没有第二验证者或全21重复验收。整体仍UNMET_INDEX，不能宣称全局完成。

当前a3冻结candidate content:3696ee9ff0d24de64db6cd9e48b2e5b6493172817333b450e1d46c4e73fd7211（源码与a2不变，仅三份knowledge文档变化）；1366源码SHA4c2a73a17e7cd18da0300d75c171e7dbb598eae33a6592e2e98793e79eeca3f0。唯一验证者实际核1380内容/1366源码/120证据SHA、8源码/16内容delta及21项来源，全部匹配。a1/a2不可变报告保留。

作者实际workspace6259通过/0失败/9既有忽略（28targets）；metadata/fmt/workspace Clippy退出0。Clippy有警告：bin test target73条（70重复），workspace去重总量未统计；作者零警告摘要失准，原日志及独立澄清保留。前端四门禁通过、117unit通过/0fail/0skip，lint71既有警告；55 Mermaid解析证据复用，当前3031本地链接通过。验证者独立读真实代码/回归/日志，没有另跑作者构建。

a2所属进程全部退出，验证者实际确认PG38896/daemon44456及55436监听不存在；全部执行会话终态，原10用户PG保留。两处自动审核拒绝清理未执行、未重试；缓存、停止数据库目录、备份/证据保留。

索引：旧G三文件与C备份六SHA一致，STALE。v6原生ACL检查在pipeline前失败；v7修复本任务C目录ACL后pipeline generic失败，两个effects均failed，G未变、无需恢复。C缓存新generation/full coverage不是权威发布。只读研究强推断失败在cache发布后artifact export，具体stage/errno UNKNOWN；worker cleanexit且无profile会删除详细日志，不认定空间/源码原因。

a3仅索引单次诊断已失败并停止，native-index-a3-v8=failed；保留worker日志实际定位artifact.export stage=write_artifact / err=write_temp / errno=28，G临时写入空间不足。旧G三SHA未变，无恢复必要；.tmp/args/reserve均无，所属daemon37184/worker43976已退出，未自动重试。不能追溯把v7 UNKNOWN改成ENOSPC，实际新压缩大小未输出。

剩余外部条件：用户腾出G空间并回复真实可用空间；新异步状态问题pending。128MiB仍只是先前保守预算，不保证充分。writer已完成必要knowledge状态与a3冻结报告，所有执行工具静止；源码4c2/21APPROVE/6259与117有效结论复用。同一backend已核索引失败证据及三份文档增量APPROVE；无第二验证者，无重跑源码门禁。IMPLEMENT_ALL与FINAL_EVIDENCE因真实空间阻塞登记blocked；运行任务/所属后台/pending effects均0，交接已保存，整体仍未完成。

## repair-r1 历史条目

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

## 历史 Git 交付授权与约定

2026-10-07用户明确授权当前修复提交并push。目标为既有GitHub仓库404F0X/Conduit-API的origin/main，采用普通推送。范围为已批准源码、知识文档和本TODO；保留原有AGENTS/README及未授权文件，运行账本、缓存、凭据和原始日志不随源码提交。代码验收结果复用，不新增验证者或重复门禁。

本文件及知识文档内指向.codex-workflow的链接是当前工作区本机审计证据，云端新checkout不包含这些运行附件；用户操作流程、项目知识及执行反思位于docs/knowledge。实际提交/推送SHA及交付结果登记在本机唯一账本和交付报告，避免在被提交文档中嵌入自身commitSHA。权威图STALE/ENOSPC待办继续保留。

## repair-r1 原索引未完成事项与历史工作

- [x] 21项源码及必要生产调用方/回归、配置/fixture/docs修复；唯一验证者a1通过17项、a2通过4项，不全21重复验收。
- [x] workspace6259/0/9、frontend117/0/0、适用门禁、55 Mermaid及当前3031links，原FAIL/忽略保留。
- [x] a2最终1380内容/1366源码/120证据manifest冻结并独立匹配，所有a2所属资源实际退出。
- [x] a3单次profile诊断保留日志，已定位本次G临时写入ENOSPC，原G/备份完好、所属进程退出；没有自动重试。
- [x] 原空间条件已被后续事实取代：用户手工清理并回报目标不存在；r3作者已确认G/C可用空间。权威图仍STALE，索引发布及identity/freshness/coverage另待明确重规划。
- [x] 同一backend核a3失败证据/三知识文档deltaAPPROVE，复用21源码/工程结论；无第二验证者。
- [ ] 显式重规划仅权威索引发布：当前空间不再沿用旧ENOSPC判断；原两次返工计数保留，不自动第三返工或换ID重置。新索引成功后只核索引增量，不重新验收源码。
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
