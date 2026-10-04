# 决策模型与本 fork

[English](DECISION_MODELS.md)

资料核查于 2026-10-03。尚未调用真实 Jev/Clef、运行付费评测或用真实账号实验。ma.4 增加了默认关闭的实验性工具搜索辅助排序；尚无真实任务加速或订阅额度节省的测量结果。

## 哪些方向有机会

Jev/Clef 面向有限选项、评分和概率判断。它们不能替代 Codex 主模型的文本生成、代码编辑或工具参数生成。安装 TypeSafe 的编程技能，是帮助开发者正确使用其 API，并不让 Codex 本体自动加速。[官方说明](https://docs.typesafe.ai/introduction/coding-agents)

结合本 fork 的源码，优先研究三个小范围方向：

| 实验              | 期望收益                         | 必须保留                               | 成功条件                             |
| ----------------- | -------------------------------- | -------------------------------------- | ------------------------------------ |
| 工具/技能语义排序 | 减少选错工具和失败的检索         | 完整工具可达性、权限和稳定的提示词前缀 | 相关工具召回更好，端到端耗时更低     |
| 可选推理强度建议  | 简单任务采用足够的推理预算       | 用户明确设置的强度与复杂任务质量       | 验收质量相当，主模型工作量降低       |
| 只读账号意图分流  | 清晰的查看请求直接返回，不走推理 | 混合意图与编码任务；修改必须明确操作   | 正确处理更多只读请求，不吞掉编码任务 |

这些判断对应 `tools/spec_plan.rs`、`tools/router.rs`、`tools/handlers/tool_search.rs`、`session/turn.rs` 和主机侧手机账号命令处理路径。目前没有一个现成的昂贵辅助分类器，能直接换掉就省下一次主模型请求。若分类后主模型仍做同样的工作，反而会增加延迟。

席位归属、配额计算、重置时间、轮换、warmup、用券、幂等性和授权继续由精确代码处理。Jev 官方也说明了算术、日期、选项顺序和恶意输入方面的局限。高置信度不能代表用户已同意操作。[已知局限](https://docs.typesafe.ai/model-jaggedness/jev-1.13)

## 接入关系与成本

应单独配置并明确启用决策服务。Jev 使用 `/v1/systemone`，不是推理账号池所需的 Responses API。Clef 在 Workers AI 上提供兼容的决策输入，但鉴权与服务包装不同。两者不应成为订阅池里的主推理账号，也不应在订阅耗尽后自动充当代码生成兜底。[TypeSafe API](https://docs.typesafe.ai/api)、[Cloudflare 接口](https://developers.cloudflare.com/workers-ai/models/clef-flash/)

当前公布的每百万输入 token 价格：Jev **$0.042**、Clef-flash **$0.09**、Clef **$0.24**。按每次整个输入 5,000 token、调用 1,000 次计算，分别为 **$0.21 / $0.45 / $1.20**，尚未计部署等其他开销。输入包括状态和问题定义；价格和限额会变化。[Jev 价格](https://docs.typesafe.ai/models)、[Cloudflare 价格](https://developers.cloudflare.com/workers-ai/platform/pricing/)

Clef 权重采用 Apache-2.0，可研究本地运行；但普通文本生成服务不等于支持其专用决策接口。9B/27B 参数规模也有显著内存和计算需求，本地部署不自动意味着免费或更快。[Clef 模型卡](https://huggingface.co/Cloudflare/clef)、[flash 模型卡](https://huggingface.co/Cloudflare/clef-flash)

## 怎样验证确实更快、更省

Cloudflare 报告的决策评测中位延迟是 Clef 209.3ms、flash 38.8ms。这属于那套评测，不能直接当作 Codex 整个任务的耗时或加速倍数。[官方发布证据](https://developers.cloudflare.com/changelog/post/2026-10-01-clef-workers-ai/)

实验应同时比较任务验收质量、总耗时、主模型请求数、推理/输出 token、缓存输入和新增决策费用。保留原路径作为基线，先使用中英合成任务；真实测试需要用户选定提供商及预算。

收益必须大于决策调用的延迟、误判重试及提示词缓存损失。token 下降也不能直接换算成 Plus/Business 的五小时或周额度节省百分比：本 fork 没有公开、稳定的换算公式。

## 已实现的接入边界

- 默认关闭，明确显示决策提供商以及内容是否离开本机；本地与云端分开选择。
- 只发送有界的相关状态和候选说明，不把凭据或账号身份交给语义排序。
- 按实际模型版本、候选集和输入共享有界缓存；不反复替换工具目录，不重写会话历史。
- 短超时后回到原流程。无效答案、未知 ID、取消和低置信度不阻断正常工作。
- 决策结果不能授权、删账号、兑换券或开启付费兜底。
- 先用不改变执行结果的观察模式，再逐项验证质量和延迟，达到门槛后才启用狭窄功能。

官方技能建议示例保留原技能目录，只增加建议，这种设计值得参考；其 Hermes/Haiku 实验并不是 Codex 编码任务的验收结果。[技能建议实验](https://docs.typesafe.ai/cookbooks/skill_suggestion)

## 在管理界面接入 Jev / Clef

新版管理器提供独立的 **决策辅助** 设置入口，无需手写 TOML 或配置终端环境变量：

1. 运行 `codex account manage`，点 **选择决策服务**；终端运行 `codex account manage --tui` 后按 **D**。
2. 选择 **Cloudflare Clef**，填写 Workers AI 页面提供的 **Account ID** 和独立 **API 令牌**。接口地址自动生成；默认模型为 Clef-flash。选择 TypeSafe Jev 时使用其独立服务令牌。
3. 选择 **仅观察** 或 **启用**，可单独开启技能建议，然后点 **保存设置**。保存本身不会调用云服务。

**测试连接** 是独立按钮：仅发送内置示例，不发送会话、账号或工作区信息，也不会保存草稿。即使关闭辅助功能，也可主动测试；测试可能产生服务费用。成功只证明本次示例请求有效，不证明所有运行中的宿主已采用设置，也不证明实际任务更快。

令牌保存在独立的服务凭据目录或配置的凭据存储中，不加入推理账号池，不回显、不放入浏览器存储，也不写入 `config.toml`。令牌绑定提供商和完整接口目标；改变 Account ID 或服务地址后，需为新目标提供令牌。已有环境变量配置继续兼容；选用已保存令牌后，缺失时不会借用环境变量、主登录或订阅账号。临时凭据存储模式的令牌不会跨重启保留。

更新后的宿主在下一次工具搜索或主会话技能建议时读取新设置，无需为每次修改重启。已经发出的请求不会取消；运行中的旧版宿主需先更新并重启一次。项目或会话的更高优先级配置保持有效，受管网络策略继续生效。设置页显示的是已保存配置，并明确提示覆盖情况，不能把它当作所有执行进程的实时确认。

关闭只需把模式改为 **关闭**；删除已保存令牌会同时关闭决策辅助。高级设置保留自定义兼容接口、超时和置信度，普通 Cloudflare 接入无需展开。

## 使用实验性辅助排序

当前实现包含**延迟加载工具搜索排序**与**单独选择启用的技能建议**。推理强度建议和账号意图分流仍是研究方向。工具排序不改目录或历史；技能建议最多追加一条短提示，不重写已有消息，不改变权限或账号轮换。`off` 继续原有 BM25 搜索，无新增 API 调用；`shadow` 调用决策服务，但返回原有结果；`rank` 使用经过校验的排序，失败时返回原有结果。两种启用模式都会向所配置的端点发送搜索 query 和候选工具说明，并可能产生独立服务费用。工具说明本身可能包含工作区信息，请先选择适合的服务。

建议先在 `CODEX_HOME/config.toml` 配置 TypeSafe 观察模式：

```toml
[decision_advisor]
mode = "shadow"
provider = "typesafe"
model = "jev-1.13.0"
api_key_env = "TYPESAFE_API_KEY"
timeout_ms = 650
min_confidence = 0.35
```

在运行 Codex 的进程中设置该环境变量。不要把密钥直接写入配置，也不能复用 `OPENAI_API_KEY` 或 `CODEX_ACCESS_TOKEN`。TypeSafe 默认端点为 `https://api.typesafe.ai/v1/systemone`。固定模型版本更便于比较；提供商别名可能在本地配置不变时指向不同模型。

Cloudflare 使用单独的 Workers AI token 和完整账号端点：

```toml
[decision_advisor]
mode = "shadow"
provider = "cloudflare"
endpoint = "https://api.cloudflare.com/client/v4/accounts/YOUR_ACCOUNT_ID/ai/run/@cf/cloudflare/clef-flash"
model = "clef-flash"
api_key_env = "CLOUDFLARE_AI_TOKEN"
timeout_ms = 650
```

若用较大模型，路径末尾与 model 同时改为 `clef`。此适配器处理 Workers AI 的 `result` 包装，不能当作 chat-completions 接口。[REST token 设置](https://developers.cloudflare.com/workers-ai/get-started/rest-api/)

可先创建合成目录 `decision-tools.json`：

```json
[
  { "name": "source_search", "description": "Find code in repositories" },
  { "name": "weather", "description": "Check the weather forecast" }
]
```

然后执行：

```sh
codex decision-advisor status
codex decision-advisor probe --query "查看明天天气" --catalog decision-tools.json
```

`status` 不向决策服务发请求，也不显示密钥值；启用后的 `probe` 会明确发起一次调用，返回合法排序或回退原因，以及此进程的匿名计数。正常 `tool_search` 使用同一套适配器。调试日志记录请求、缓存命中、超时、接受的排序、实际应用和回退数量，不记录 query、候选文本、URL 或凭据。这些是当前进程的计数，不能代表另一个 daemon，也不能代替整项任务评测。

先与 `mode = "off"` 比较实际任务质量和总耗时；确认有益后再改成 `mode = "rank"`。新版进程在下一次辅助调用前读取修改；旧进程需更新并重启一次。关闭只需将 mode 改回 `off`。

请求最多含 32 个候选，每条说明最多 768 个 UTF-8 字节，query 最多 2,048 字节，编码后的请求和响应各不超过 32 KiB。小目录可以完整进入语义判断，中文和同义词搜索因而可能找到英语词法检索漏掉的工具。大目录结合词法候选与少量确定性的覆盖采样，**不保证全目录语义召回**。总超时默认 650 ms，可设置为 100–1,500 ms；最多两次并发调用。没有自动付费重试。取消会结束等待与网络工作，但已送达的请求仍可能被提供商收费。

每个进程最多缓存 128 项，成功结果缓存 60 秒；键涵盖提供商、模型、设置、完整目录文本修订、候选集、query 和凭据摘要。同输入并发共用一次请求。无效回答和服务故障短暂缓存，以减少立即重复调用；每次仍检查当前应用网络策略，辅助服务无法扩大网络权限。云端要求 HTTPS，拒绝重定向。

如果已经有兼容 System One 的本地服务，可显式设置 `http://127.0.0.1:PORT/v1/systemone`、`allow_local_http = true`；无鉴权服务可以设 `api_key_env = ""`。普通本地文本生成端点并不足够。本 fork 不下载权重、不部署模型，也不启动模型服务。

## 单独启用技能建议

如希望同一服务帮助选择技能，在已有 `[decision_advisor]` 中加入 `suggest_skills = true`。默认是 `false`；普通工具搜索排序不会自动开启此功能。先保持 `mode = "shadow"` 观察，再决定是否改为 `rank`。

此功能在主用户会话的每个 turn 边界最多增加一次请求，发送该 turn 最后提交的文本及最多 32 条符合条件的主机技能名称/说明。没有发送完整对话、SKILL.md 正文、结构化路径或凭据；技能说明本身仍可能包含敏感信息。只考虑已发现、已启用、允许隐式调用、符合当前产品限制且名称没有歧义的技能。明确选择或提到技能时跳过建议；守护审核、内部任务和子代理会话不会发出此请求。关闭技能目录说明时也跳过。

`shadow` 只记录匿名比较与耗时，不写入上下文。`rank` 在明确技能处理完成后最多追加一条 **1,200 字节**的上下文提示，包含一个已经存在的技能名称与遵守用户、AGENTS、技能规则和权限的提醒。它不会自动读技能正文、安装依赖、启用应用或执行技能；主模型仍按正常工作流判断是否需要读取。已有历史和技能目录保持原样。无匹配、低置信度、无效返回、取消或超时都不添加提示。

这是可用的实验性建议流程，尚无真实质量/速度增益证据。当前覆盖主机发现的技能，不包含独立云端或执行器技能目录的语义建议。大目录有界采样也可能漏掉合适技能。建议会新增少量主模型上下文，并可能带来一次决策服务费用与等待；请同时比较正确选技率、返工、主模型总工作量与完整任务时间。取消此功能只需 `suggest_skills = false`，无需关闭工具搜索排序。
