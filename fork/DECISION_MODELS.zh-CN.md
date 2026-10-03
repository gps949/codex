# 决策模型与本 fork

[English](DECISION_MODELS.md)

资料核查于 2026-10-03。尚未调用真实 Jev/Clef、运行付费评测或用真实账号实验。下述接入方案属于研究设计，尚未成为默认启用的加速功能。

## 哪些方向有机会

Jev/Clef 面向有限选项、评分和概率判断。它们不能替代 Codex 主模型的文本生成、代码编辑或工具参数生成。安装 TypeSafe 的编程技能，是帮助开发者正确使用其 API，并不让 Codex 本体自动加速。[官方说明](https://docs.typesafe.ai/introduction/coding-agents)

结合本 fork 的源码，优先研究三个小范围方向：

| 实验              | 期望收益                         | 必须保留                               | 成功条件                             |
| ----------------- | -------------------------------- | -------------------------------------- | ------------------------------------ |
| 工具/技能语义排序 | 减少选错工具和失败的检索         | 完整工具可达性、权限和稳定的提示词前缀 | 相关工具召回更好，端到端耗时更低     |
| 可选推理强度建议  | 简单任务采用足够的推理预算       | 用户明确设置的强度与复杂任务质量       | 验收质量相当，主模型工作量降低       |
| 只读账号意图分流  | 清晰的查看请求直接返回，不走推理 | 混合意图与编码任务；修改必须明确操作   | 正确处理更多只读请求，不吞掉编码任务 |

这些判断对应 `tools/spec_plan.rs`、`tools/router.rs`、`tools/handlers/tool_suggest.rs`、`session/turn.rs` 和主机侧手机账号命令处理路径。目前没有一个现成的昂贵辅助分类器，能直接换掉就省下一次主模型请求。若分类后主模型仍做同样的工作，反而会增加延迟。

席位归属、配额计算、重置时间、轮换、warmup、用券、幂等性和授权继续由精确代码处理。Jev 官方也说明了算术、日期、选项顺序和恶意输入方面的局限。高置信度不能代表用户已同意操作。[已知局限](https://docs.typesafe.ai/model-jaggedness/jev-1.13)

## 接入关系与成本

应单独配置并明确启用决策服务。Jev 使用 `/v1/systemone`，不是推理账号池所需的 Responses API。Clef 在 Workers AI 上提供兼容的决策输入，但鉴权与服务包装不同。两者不应成为订阅池里的主推理账号，也不应在订阅耗尽后自动充当代码生成兜底。[TypeSafe API](https://docs.typesafe.ai/api)、[Cloudflare 接口](https://developers.cloudflare.com/ai/models/%40cf/cloudflare/clef-flash/)

当前公布的每百万输入 token 价格：Jev **$0.042**、Clef-flash **$0.09**、Clef **$0.24**。按每次整个输入 5,000 token、调用 1,000 次计算，分别为 **$0.21 / $0.45 / $1.20**，尚未计部署等其他开销。输入包括状态和问题定义；价格和限额会变化。[Jev 价格](https://docs.typesafe.ai/models)、[Cloudflare 价格](https://developers.cloudflare.com/workers-ai/platform/pricing/)

Clef 权重采用 Apache-2.0，可研究本地运行；但普通文本生成服务不等于支持其专用决策接口。9B/27B 参数规模也有显著内存和计算需求，本地部署不自动意味着免费或更快。[Clef 模型卡](https://huggingface.co/Cloudflare/clef)、[flash 模型卡](https://huggingface.co/Cloudflare/clef-flash)

## 怎样验证确实更快、更省

Cloudflare 报告的决策评测中位延迟是 Clef 209.3ms、flash 38.8ms。这属于那套评测，不能直接当作 Codex 整个任务的耗时或加速倍数。[官方发布证据](https://developers.cloudflare.com/changelog/post/2026-10-01-clef-workers-ai/)

实验应同时比较任务验收质量、总耗时、主模型请求数、推理/输出 token、缓存输入和新增决策费用。保留原路径作为基线，先使用中英合成任务；真实测试需要用户选定提供商及预算。

收益必须大于决策调用的延迟、误判重试及提示词缓存损失。token 下降也不能直接换算成 Plus/Business 的五小时或周额度节省百分比：本 fork 没有公开、稳定的换算公式。

## 拟采用的接入约束

- 默认关闭，明确显示决策提供商以及内容是否离开本机；本地与云端分开选择。
- 只发送有界的相关状态和候选说明，不把凭据或账号身份交给语义排序。
- 按实际模型版本、候选集和输入共享有界缓存；不反复替换工具目录，不重写会话历史。
- 短超时后回到原流程。无效答案、未知 ID、取消和低置信度不阻断正常工作。
- 决策结果不能授权、删账号、兑换券或开启付费兜底。
- 先用不改变执行结果的观察模式，再逐项验证质量和延迟，达到门槛后才启用狭窄功能。

官方技能建议示例保留原技能目录，只增加建议，这种设计值得参考；其 Hermes/Haiku 实验并不是 Codex 编码任务的验收结果。[技能建议实验](https://docs.typesafe.ai/cookbooks/skill_suggestion)
