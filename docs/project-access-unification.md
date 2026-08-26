# Project 访问权益统一

状态：已完成
日期：2026-08-12

## 目标

保留现有简单模式与企业模式功能，但只保留一条客户访问权限链：

```text
Project 基础 Access Plan
  + 有效 Subscription Access Grant
  + 管理员 Project Grant
  - 管理员 Project Block
  -> Effective Project Access
  -> API Key 模型/渠道限制只能继续收窄
  -> 模型目录、/v1/models 与实际路由共同消费
```

Simple Group 是 Access Plan、Price Tier 与成员 Project 的简单模式包装。Role/Scope
继续只管理“谁能管理什么”，不承担客户模型或渠道权益。

## 必须保留的产品能力

- Simple Group 配置可用模型、可用渠道、零售倍率和成员用户。
- 订阅提供周期额度并附加模型/渠道访问权益，多订阅按来源叠加。
- API Key 限制模型、渠道、有效时间、请求/Token/费用额度、映射和负载均衡策略。
- 用户模型目录只显示实际可调用的模型及对应渠道。
- 企业渠道、Offer、优先级、权重、健康检查、采购价格和上游额度探测保持独立。
- Project、Role、Scope、Request、Trace、Thread、Prompt、Data Storage 等企业能力保持不变。

## 明确不做

- 不迁移测试数据库中的旧 User Group、订阅 Grant、User Credit 或 API Key `groupIDs`。
- 不保留旧 Group 双读、旧字段只读兼容、Shadow 对比或迁移状态。
- 本切片不同时开启真实钱包扣费；Reserve/Capture/Release 单独实施。

## 权益语义

- Access Plan Version 同时保存模型集合和渠道集合。
- 一个来源允许的路由是该版本模型与渠道在有效 Channel Offer 上的组合。
- Project 的多个有效来源按路由组合取并集，显式 Block 最后生效。
- API Key 的空模型/渠道限制表示继承 Project；非空限制与 Project 结果取交集。
- 禁用模型、禁用渠道或禁用 Offer 永远不能被客户权益重新启用。

## 实施 TODO

### A. Schema

- [x] Access Plan Version 增加渠道项。
- [x] Subscription Plan 改为关联 Access Plan。
- [x] 增加来源可追踪的 Project Access Grant。
- [x] 删除旧 User Group、Group Entitlement、Group Price Modifier 和 Subscription Group Grant Schema。
- [x] 删除 Simple Group 独立渠道表；保留仍用于简单模式默认方案的订阅关联。

### B. 运行时

- [x] 实现唯一 `EffectiveProjectAccessResolver`。
- [x] 模型目录、API Key 校验、`/v1/models` 和候选路由使用同一解析结果。
- [x] Simple Group 模型/渠道编辑发布同一个 Access Plan Version。
- [x] 订阅分配、暂停、恢复、取消、续期维护 Project Access Grant。
- [x] API Key `channelIDs` 成为正式收窄能力，删除 `groupIDs`。

### C. API 与前端

- [x] Billing 计划编辑从旧 Group 切换为 Access Plan/Simple Group。
- [x] 删除旧 User Group GraphQL 契约；Credit 与 Wallet Comparison 作为现有额度展示能力保留。
- [x] API Key 页面只展示当前 Project 可用的模型与对应渠道。
- [x] 保留企业 Offer、Price Book、渠道治理，移除其中旧 Group 策略与应收模拟区域。

### D. 验证

- [x] Access Plan、订阅 Grant、管理员 Block 与 Key 限制组合测试。
- [x] `/project/models`、`/v1/models` 和真实请求共享同一 Project 路由结果。
- [x] 多订阅生命周期按来源撤销，不影响其他来源。
- [x] PostgreSQL Schema 检查。
- [x] 聚焦测试、workspace fmt 和相关前端检查通过。

## 后续独立切片

Project Wallet 正式执行：Quote -> Reserve -> Capture/Release；订阅额度优先，Project
Credit 后备。采购成本与客户销售金额分别记录。
