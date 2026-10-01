# 多账号 Codex 使用指南

[English](README.md) · [下载发布版](https://github.com/gps949/codex/releases) · [维护说明](../FORK_MAINTENANCE.md)

此 fork 将多个 ChatGPT 账号或 Business 席位放进同一个本地账号池。Codex 自动选择可用账号，在配额不足时切换，也可以等待额度恢复后继续任务，减少手动退出和重新登录。每个配置档对应特定的 ChatGPT 用户与工作区；同一邮箱的个人账号和 Business 账号可以分别加入。

## 先加入两个账号

Apple Silicon macOS、Linux x64/arm64 可以运行：

```sh
curl -fsSL https://raw.githubusercontent.com/gps949/codex/feature/native-multi-account/install.sh | bash
codex --version
```

版本号包含 `+ma.N`。Linux 发行包需要系统 OpenSSL 3。Windows x64/arm64 请从[发布页](https://github.com/gps949/codex/releases)下载对应 ZIP，完整解压，将所在目录加入 PATH。所有随包辅助程序都需要放在 `codex` 或 `codex.exe` 同一目录下。

为账号取简短、容易辨认且不重复的名称：

```sh
codex account add --label "Personal"
codex account add --label "Work"
codex account list
codex
```

每次登录时选对账号与工作区。首次创建账号池会登记已有的根 ChatGPT 登录；重复添加同一用户、同一工作区会更新原配置档。远程主机没有浏览器时可运行 `codex account add --label "Work" --device-auth`。

用 `account list` 核对最终名称。如果导入的账号仍叫 Existing login，可以先重命名，再使用下文示例：

```sh
codex account set "Existing login" --label "Personal"
```

添加成功后账号池自动生效，继续正常使用 Codex 即可。输入 `/account` 可以查看账号与选择方式。

## 日常操作速查

| 想做什么         | 主机 CLI                                                    | Codex TUI                               | 手机远程端                         |
| ---------------- | ----------------------------------------------------------- | --------------------------------------- | ---------------------------------- |
| 看有哪些账号     | `codex account list`                                        | `/account`                              | `/account` 或 `/account list`      |
| 看配额           | `codex account status`（别名：`pool`）                      | `/status` 与 `/account`                 | `/status`；`/account show "Work"`  |
| 选择账号         | `codex account use "Work"`                                  | 在 `/account` 中选择 Work               | `/account use "Work"`              |
| 交给调度器选择   | 在 Codex 中打开 `/account`                                  | 在 Accounts 页选择 Choose automatically | `/account auto`                    |
| 调整轮换策略     | `codex account config set-rotation-strategy earliest-reset` | 在 Strategy 页选择策略                  | `/account strategy earliest-reset` |
| 查看账号池设置   | `codex account config show`                                 | 打开 `/account` 的 Help 页              | `/account settings`                |
| 修复登录         | `codex account login "Work"`                                | 在主机运行 CLI 命令                     | 在主机运行 CLI 命令                |
| 暂留一个账号不用 | `codex account disable "Work"`                              | 在主机运行 CLI 命令                     | 在主机运行 CLI 命令                |
| 重新加入调度     | `codex account enable "Work"`                               | 在主机运行 CLI 命令                     | 在主机运行 CLI 命令                |

手动选择对后续请求生效，自动故障切换仍然开启；它不会永久将一个对话固定在该账号。使用同一个 `CODEX_HOME` 的进程也会共享当前选择与冷却状态。

名称含空格时加引号。CLI 支持完整配置档 ID 或唯一名称；需要脚本操作或名称重复时，用 `codex account list --show-profile` 查看 ID。手机端可直接使用 `/account list` 给出的 `@selector`，也适用于邮箱相同的多个账号。

CLI 的 `use` 和手机端 `/account use` 会遵守额度冷却。确认账号额度已经恢复后，可以用 `codex account use "Work" --force` 或手机端 `/account retry "Work"` 明确重试。它仅清除本地冷却以便重新探测，服务端的真实配额限制仍然有效。TUI 中冷却账号会明确标为 `Retry`，选中后可查看重试说明。

手机 `/status` 弹框的进度条标题明确表示当前账号的额度；账号栏预览池内可用数量、当前账号及一个备用账号，使用短标签和可用状态。其他账号通过 `/account` 分页查看。百分比仍属于各自账号，不会被相加成“账号池总余额”。不同原生 App 版本可能限制预览文字的可见长度。

TUI 的 `/account` 窗口分为 **Accounts**、**Strategy**、**Help** 三页。按底部键位提示切换页面、选择项目，输入文字可筛选名称或邮箱。Help 解释当前的额度保留、等待续跑、预热、reset-credit 设置；暂留或需要重新登录的账号会给出主机 CLI 恢复命令。

## 配额用得更充分，账号池可用得更久

默认策略 **fill-first** 按优先级使用账号，数字越小越优先。例如：

```sh
codex account set "Personal" --priority 0
codex account set "Work" --priority 10
```

**earliest-reset** 优先选择观察到的重置时间更近的可用账号，并优先启动尚未开始的主窗口倒计时。整天持续使用时，可以尝试这一策略。观察到的重置时间与额度不等于保证可用的时长；切换策略的命令见上表。

有其他可用账号时，Codex 默认在观察到的使用率到 **95%** 后提前切换。保留下来的额度仍可在其他账号耗尽后作为备用额度重新使用。更改阈值会改变余量与切换次数，不会增加账号配额。

**备用账号预热**默认开启：向符合条件的空闲备用账号发送少量生成请求，尝试提前开启主窗口倒计时，同时保留任务当前使用的身份。维护请求采用模型支持的最低推理强度，并要求仅返回一位数字，但仍会消耗配额。已完成或可能中断的尝试受到五小时防重复生成保护；期间可以只查询额度来确认结果。临近切号时，预热检查可加快到每五分钟。持续使用时可保留开启；更在意备用额度完全不动、能够接受稍后才启动倒计时时，可以关闭。

模型专属限额不会停用整个账号，可以按后端提示切换模型继续使用。某个账号的套餐或权限不包含本次使用时，Codex 会尝试其他可用账号，并记录短暂冷却；这类权限错误不会消耗 reset credits。后端工作区级限制仍可能同时影响多个席位，即使各席位显示的用量百分比不同。

**Reset credits** 默认保留，不会自动兑换。自动兑换需要主动开启，只在全池耗尽且最近的自然重置还超过设定等待门槛时考虑。最后失败账号没有信用次数时会继续检查其他耗尽账号，确认恢复即停止；兑换结果不明确时暂停，不再兑换其他账号。手动兑换会消耗该账号有限的信用次数；刷新额度或使用 `--force` 选择账号都不会产生新配额。

所有账号耗尽后，Codex 可以**等待恢复并安全续跑**，默认最长等待六小时。原主机进程需要一直运行；结束进程会结束等待。用户可随时取消。已经显示的部分输出或尚未处理完整的工具结果可能需要先核对，无法直接自动续跑。

手机端标为 **Used** 和 CLI 的 `5H%` / `WEEK%` 表示已用；TUI 中带 **left** 的标签表示剩余百分比。Primary 通常是滚动五小时窗口，Secondary 通常是每周窗口，任一窗口都可能限制账号。需要分别看每个账号的两个窗口；不同套餐的百分比相加无法得到有意义的账号池余额。未知和缓存数值仅是观察结果，刷新失败不表示账号使用率为 0%。

## 理解备用账号预热

自动预热需要 Codex 会话或连接中的 app-server 保持运行；一次性的 `codex account status` 查询不会让调度器持续运行。首次自动检查约等待 30 秒，每轮最多检查一个备用账号，默认间隔五分钟。多个备用账号会在后续轮次逐步处理。生成请求不会选中当前账号、已禁用账号、需要重新登录的账号、额度耗尽的账号或仍受尝试保护的账号。共享 `CODEX_HOME` 的进程会协调预热，避免各自向同一个账号重复生成。

生成请求完成与观察到五小时窗口启动是两件事。微量请求完成后，用量仍可能显示 0%；空闲账号也可能收到一个重置时间，而实际倒计时尚未开始。`start unconfirmed` 表示尚未确认启动，不会直接断言“还没开始”。主窗口出现正用量，才说明观察到了窗口启动；周窗口倒计时不能作为主窗口启动的证据。超过一天的倒计时带有明确单位，例如 `4d 3h 43m` 表示四天三小时四十三分钟。

| 预热状态                            | 含义                                                                                         |
| ----------------------------------- | -------------------------------------------------------------------------------------------- |
| `warmup in progress`                | 最近的一次尝试正在执行，任务使用的当前身份保持不变。                                         |
| `warmup sent; start unconfirmed`    | 生成请求已经发出，可能已完成或中断，但主窗口用量尚未确认启动；可以继续查额度，无需再次生成。 |
| `warmup attempt; start unconfirmed` | 之前的尝试可能中断；确认结果期间仍保留防重复生成保护。                                       |
| `warmup failed; retry in …`         | 请求失败后正在退避，时间表示最早重试时间，不是额度重置时间。                                 |
| `warmup deferred; check in …`       | 预检发现账号暂不符合条件，没有发送生成请求。                                                 |
| `warmup retry ready`                | 失败退避已结束，后续符合条件的检查可以再试。                                                 |
| `warmup needs login`                | 使用 `codex account login "Work"` 修复对应账号的登录。                                       |
| `5h confirmed`                      | 最近的预热状态与主窗口正用量一致。                                                           |

TUI 内输入 `/warmup` 可查看生效设置、任务是否运行、每个账号的候选或跳过原因、持久化尝试与近期事件。事件只属于主机当前进程，尝试保护状态可以跨重启保留。手机端输入 `/account warmup` 查看摘要。两者均为只读诊断，不会发送生成请求。

TUI 的 `/warmup now` 或手机端的 `/account warmup now` 请求立即检查一次。它们遵守预热关闭设置、账号资格和现有退避；需要生成时仍会消耗配额。命令会在后台检查完成前返回，稍后再查看摘要即可。反复执行不会绕过保护期。关闭预热可在主机运行 `codex account config set-window-warmup false`，或在手机输入 `/account warmup off`。

## 调整设置

`codex account config show` 可查看生效的设置。CLI 设置命令将值写入 `${CODEX_HOME}/config.toml`（通常为 `~/.codex/config.toml`）的 `[account_pool]` 部分，也可以直接编辑该部分。下列值是默认值：

```toml
[account_pool]
rotation_strategy = "fill_first" # or "earliest_reset"
return_to_preferred = true
preemptive_switch_percent = 95
window_warmup = true
window_warmup_interval_minutes = 5
resume_after_reset = true
max_reset_wait_minutes = 360
auto_reset_credits = "never" # or "when_pool_exhausted"
auto_reset_credit_min_wait_minutes = 60
```

| 设置                                 | 含义                                                                      |
| ------------------------------------ | ------------------------------------------------------------------------- |
| `return_to_preferred`                | 优先账号冷却结束后返回该账号。设为 `false` 可继续使用当前仍然可用的账号。 |
| `preemptive_switch_percent`          | 提前切换阈值。设为 `0` 关闭提前切换，达到硬限制后仍然可以故障切换。       |
| `window_warmup_interval_minutes`     | 后台预热检查间隔，小于 5 的值按 5 分钟处理。                              |
| `resume_after_reset`                 | 为已耗尽的账号池任务开启可取消的等待。                                    |
| `max_reset_wait_minutes`             | 单次任务最长等待时间，上限 1440 分钟；`0` 关闭等待。                      |
| `auto_reset_credits`                 | `never` 保留信用次数，`when_pool_exhausted` 开启前述规则限定的兑换。      |
| `auto_reset_credit_min_wait_minutes` | 最近自然重置在此分钟数内时跳过自动兑换。                                  |

每项设置都可以用 CLI 命令调整：

```sh
codex account config set-rotation-strategy earliest-reset
codex account config set-return-to-preferred false
codex account config set-preemptive-switch-percent 95
codex account config set-window-warmup false
codex account config set-window-warmup-interval-minutes 10
codex account config set-resume-after-reset true
codex account config set-max-reset-wait-minutes 360
codex account config set-auto-reset-credits never
codex account config set-auto-reset-credit-min-wait-minutes 60
```

这些是互相独立的示例。更改后通过 `config show` 核对实际生效值，更高优先级的配置或启动覆盖项可能影响结果。CLI 预热间隔允许 5–1440 分钟，等待时间允许 0–1440 分钟。

更在意持续使用，可尝试 `earliest_reset` 并开启预热；更在意保存备用配额，可保留 `fill_first` 并设置 `window_warmup = false`；不希望中途返回优先账号，可设置 `return_to_preferred = false`。先调整一项，再通过 `/account` 观察效果。

## 手机远程使用

先在远程主机配置账号，然后用安装的 fork 启动远程控制：

```sh
codex remote-control start
```

按输出提示配对。需要时，`codex remote-control pair` 可生成短时有效的配对码。远程控制身份与主机根登录关联，执行账号池轮换不会要求手机重新配对。

在支持远程功能的 ChatGPT iOS 或 Android 客户端里，可以输入：

```text
/account help
/account list 2
/account show "Work"
/account use "Work"
/account auto
/account strategy
/account settings
/account warmup off
/account resume on
/account wait 360
/account reset-credits never
```

列表每页显示四个账号。复制列表给出的 `@selector` 可以准确查看或选择账号。单账号详情只刷新该账号。`/account` 与 `/status` 在主机本地处理，不发送模型推理请求，也不进入模型对话历史；查询仍然可能发送额度元数据请求。任务运行期间，请使用应用的 Status 面板，或等当前任务结束后再输入这些聊天命令。

fork 通过手机端已有界面提供账号池标题、状态回复与控制，无法替换应用的原生页面。具体表现可能随客户端版本变化，iOS 重连仍待实机验收。

## 状态怎么看，遇到问题怎么处理

| 状态或现象                          | 下一步                                                                                                                   |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Available / ready                   | 可参与调度，观察到的额度仍可能未知或过期。                                                                               |
| Cooling down / exhausted            | 等待重置，或让调度器使用其他账号。                                                                                       |
| Login required / auth unavailable   | 运行 `codex account login "Work"`，使用原用户与原工作区登录。                                                            |
| Disabled                            | 账号被暂留；运行 `codex account enable "Work"` 重新参与调度。                                                            |
| Cached / unknown 配额               | 再次查看账号；保留的缓存不代表刚检查成功。                                                                               |
| 退出登录后账号池持续暂停            | 用 `codex account use "Work"` 明确选择账号以恢复。                                                                       |
| 更新后 `/account` 不可用            | 确认 PATH 中程序版本含 `+ma.N`，运行 `codex app-server daemon stop` 停止受管理后台，再启动；手机使用时重新启动远程控制。 |
| 旧对话提示 opaque compacted history | 先切回历史所属账号运行 `/compact`，再使用另一个账号。                                                                    |

重新登录会先暂存新凭据，核对同一用户与工作区后再替换；要添加不同用户或席位，请用 `account add`。修复登录通常无需删除账号再重新添加。

`codex logout` 会暂停账号池，并在重启后保留暂停，已登记配置档继续保留。`codex account remove "Work"` 删除一个配置档，通常也会删除或撤销其存储的凭据；加 `--keep-credentials` 可保留凭据。仅想暂留账号时使用可恢复的 `disable`。

## 更新

再次运行 fork 安装脚本，或下载较新的对应平台发行包。指定版本时在命令后加准确标签：

```sh
curl -fsSL https://raw.githubusercontent.com/gps949/codex/feature/native-multi-account/install.sh | bash -s -- rust-vX.Y.Z-ma.N
```

macOS/Linux 安装脚本会检查发布的 SHA256，将主程序与辅助程序一起安装，并停止过期的受管理后台。手机用户需要随后重新启动远程控制。`codex update` 会引导至 fork 发布页。

账号配置档与凭据存放在主机 `CODEX_HOME` 内，凭据遵循文件或系统钥匙串配置，请妥善保管该目录。配置 API-key provider 不会将 API key 转变为订阅账号池配置档。

跨工作区并发路由、旧不透明压缩历史自动迁移，以及真实个人/Business/手机使用仍需要后续完善或验收。自动化模拟用例与发行包完整性检查无法代替这些真实账号场景。
