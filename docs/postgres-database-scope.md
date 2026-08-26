# PostgreSQL 数据库边界与执行设计

> 更新日期：2026-08-20
>
> 本文只描述 PostgreSQL 原生运行和性能主线，不改变 Conduit API 的企业权限、模型组、订阅或渠道产品语义。

## 当前实现

Rust 工作区已经收敛为 PostgreSQL-only：PostgreSQL driver、migration、`Pg*Repo` 和 production wiring 始终编译，不再存在数据库 feature 选择。`build_postgres_core_services` 创建 `PostgresPools`；主库承担全部写入、事务和强一致读取，可选只读 pool 只提供给 Operations/Dashboard 等明确允许最终一致的查询。

因此，默认状态是“单主库原生运行”；只有显式配置 `db.read_replica.read_dsn` 时才连接副本。没有配置副本时，所有读取继续走主库。

## 一致性分类

| 类别 | 典型链路 | 连接要求 |
|---|---|---|
| 主库写入 | 所有 INSERT/UPDATE/DELETE、migration、GC、模型同步、探测写入、备份元数据 | master |
| 强一致读取 | 登录/API Key 鉴权、Quota admission、钱包余额、Reserve/Capture/Release、订阅/模型组权益、价格与路由候选、写后读取 | master |
| 事务读取 | `BEGIN` 内的 `FOR UPDATE`、advisory lock、结算和配额事务 | 事务从 master 开始，期间禁止切换 |
| 最终一致读取 | Operations/Dashboard 聚合、请求/执行/Usage 历史列表、trace/observability 列表 | 第一阶段可选 replica，失败按策略回 master |
| 谨慎读取 | 模型/渠道目录、公共健康快照、备份 inventory | 默认 master；完成延迟和写后读取验证后再分批接入 |

不能通过 SQL 文本自动判断读写一致性。一个 GraphQL resolver 或 adapter 必须显式声明使用强一致或最终一致目标；同一次聚合查询必须使用同一个 pool，避免结果跨主/副本不一致。

## 运行边界

`PostgresPools` 是 live runtime 边界，包含：

- `master: PgPool`：所有写入、事务和强一致读取的唯一入口；
- `replica: Option<PgPool>`：可选最终一致读取入口；
- `fallback_on_replica_failure`：副本不可用时是否回退主库；
- 显式 `master_pool()`、`read_pool()`/`eventual_read_pool()` 访问器。

连接层负责独立池容量、UTC、statement cache、application name 和关闭；wiring 负责把应用配置映射为主/副本连接；Repository 不自行解析 DSN 或散落副本判断。`RuntimePool` 和 maintenance 暂时继续取 master，避免把后台写任务误接到副本。

Operations/Dashboard adapter 已接入可选只读 pool。鉴权、钱包、订阅权益、模型/渠道路由和所有 billing/quota 事务不接副本。

## 性能与安全门禁

1. 以隔离 PostgreSQL schema 和可重复数据量执行 `EXPLAIN (ANALYZE, BUFFERS)`，记录请求、执行、Usage、Operations 和钱包查询的 before/after、p95、吞吐及锁等待。
2. 真实并发验证 Reserve/Capture/Release、Quota admission 和结算幂等性，确认无超扣、死锁和重复结算；验收证据必须来自 PostgreSQL。
3. 副本启动必须校验 schema version、连接权限和复制延迟；未配置副本时必须走单主库回归路径。
4. Backup/Restore 必须在隔离数据库验收。当前 PostgreSQL backup archive 会原样包含渠道 credential/API key，接入自动备份前必须脱敏或加密，且要验证坏归档回滚和备份失败状态。
5. 禁止把 DSN、明文 credential 或 API key 写入日志、诊断、性能报告或提交。

## 分阶段交付

1. **PG-ARCH-01**：本文件固化调用图和一致性分类（已完成）。
2. **PG-POOL-01**：主/副本 live pool、事务固定 master 和 fallback 策略（已完成代码与聚焦测试）。
3. **PG-READ-01**：Operations/Dashboard 最终一致读取接入副本（已完成代码与聚焦测试）。
4. **PG-PERF-01 / PG-TX-01**：补代表性数据、查询计划和并发锁等待基线。
5. **PG-BACKUP-01 / PG-RUNTIME-01**：完成安全的 Backup/Restore 生命周期和生产配置核心冒烟。

## 非目标

- 不为兼容旧数据增加迁移层；测试产品允许重建数据库。
- 不在本阶段重写全部 20 个 PostgreSQL Repository。
- 不恢复其他数据库 runtime，也不把商业化页面或简单/企业模式产品改动混入 PostgreSQL 性能主线。
