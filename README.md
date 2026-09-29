# Grok Budget 0.2.5

Grok Build 的 Rust 原生额度插件，适配 Windows 上的 Grok Build 1.0.41。读取账户真实占比、重置时间，汇总本机会话与本周期成本和 Token，并持续记录样本、估算周限。

运行、安装与看板导出均不需要 Python、Node 或 Rust；从源码编译才需要 Rust。HTTP、SQLite 与 HTML 模板包含在 `grok-budget/grok-budget.exe` 中。

## 下载

从 [GitHub Releases](https://github.com/CodingChiao/grok-budget/releases/latest) 下载 Windows x64 分发包，解压后运行 `./install.ps1`。发布页同时提供 SHA-256 校验文件。

本仓库仅包含 Grok Budget 插件及其安装、看板和测试代码，不包含 Grok Build 本体。克隆源码后请运行 `./install.ps1 -Build`；需要 Rust 工具链和 Windows C++ 构建工具。使用预编译分发包不需要编译工具。

## 使用

```powershell
./grok-budget.cmd                         # 查询
./grok-budget.cmd --refresh               # 立即刷新服务端
./grok-budget.cmd --json                  # 结构化摘要
./grok-budget.cmd --offline               # 完整历史快照，不联网、不刷新本地统计
./grok-budget.cmd --watch 10              # 持续采样，Ctrl+C 停止
./grok-budget.cmd --html output/grok-budget.html
```

双击 `看板.cmd` 生成并打开中文看板。新会话中的 `/budget` 让 Grok 调用插件解释结果，模型解释可能消耗额度；独立查询不调用模型。

## 状态栏

紧凑两列、正常亮度，不使用暗淡文字、竖线分隔或随窗口拉大的间距。会话金额与 Token 同行：金额青色、Token 品红；已用占比带字符进度条并按 80% / 95% 阈值切换绿、黄、红。窗口过窄时省略进度条，更窄时退化为单行列表。加 `--no-color` 或设 `NO_COLOR` 可强制纯文本。示例：

```text
会话 $0.53 · 1.64M Token        已用 $1.54 · 5.94M Token · ░░░░░░░░░░ 4%
周限 ≈$51.19 · ≈197.89M Token   重置 10-06 00:08
```

- 会话：优先使用 Grok 传入的金额和累计输入＋输出 Token。金额缺失时，按准确的 `session_id` 成对读取对应账本的金额和 Token，避免恢复会话传入的 0 覆盖真实累计值。不猜测最新会话，不读取聊天记录。
- 已用：本周期本机累计金额、累计 Token、服务端账户已用占比。百分比可能包含其他设备和 Chat；金额和 Token 只覆盖本机已记录轮次。
- 周限：预估美元周限和预估 Token，以 `≈` 标记，均不是官方上限。
- 重置：账户周期实际结束时间，按本机时区显示；旧快照明确标记。

未知金额显示 `$--`，未知 Token 显示 `--`，零值保留。`*` 表示成本或账本不完整。80% 起黄色提示，95% 起红色提示；`--no-color` 或 `NO_COLOR` 关闭颜色。窄窗口分为最多四行。

**状态栏每 2 秒读取本地结果，活跃会话的账户额度约每 10 秒查询，空闲会话约每 60 秒查询。** Grok 的 `state` 事件和正在处理的 `prompt_id` 表示活跃；最后一次活跃信号后保留 60 秒活跃窗口。本地累计金额和 Token 根据账本文件修改时间、大小及文件集合变化刷新，聚合缓存最多保留 10 秒；不会把新显示统计写回旧估算样本。进行中的调用仍需 Grok 写入账本才可计入。

SessionStart 提交立即查询信号；Stop 提交约 2 秒后的查询，若占比等额度信息未变化，约 10 秒后补查一次。所有自动请求至少相隔 5 秒，因此刚完成过查询时事件请求会合并或延后。状态栏显示“更新于 N 秒前”；实际可见延迟还取决于服务端记账及下一次状态栏刷新。

Windows 当前用户任务 `GrokBudgetMonitor` 在登录后隐藏运行，统一处理请求；状态栏和 hooks 只写入信号，不等待网络。最后一次状态栏/hook 信号超过 20 秒后不再自动访问额度接口。Grok 会终止状态栏的子进程，因此不能直接从状态栏派生后台查询进程。

JSON 的 `local` 为与服务端额度同次采样的历史统计，`live_local` 为本次新读取的显示统计，`local_refreshed_at` 为更新时间。显示数据不写回历史样本，不用新成本搭配旧百分比重新估算。`--offline` 只返回历史快照。

## 安装与更新

```powershell
./install.ps1                 # 使用分发包中的 Windows x64 程序安装
./install.ps1 -Build          # 从源码编译后安装
```

安装后需要重新启动 Grok 客户端以加载 2 秒刷新配置和新 hooks；安装器不会结束当前会话。现有启动器会立即切换到 0.2.5，但已经打开的客户端仍沿用原来的定时配置。

安装器注册当前用户的登录任务 `GrokBudgetMonitor`，使用普通用户权限、隐藏窗口，不需要保存密码；安装环境需允许创建当前用户计划任务。源码或解压包中的独立查询仍可直接使用；后台自动刷新需先安装。

安装器通过 Grok 的 `plugin validate/install/update/enable` 注册插件，并明确同步安装副本。若本地插件 update 未更新注册版本，通过官方 uninstall/install 重新注册并保留数据，之后校验版本。状态栏使用 `~/.grok/grok-budget/statusline.cmd` 调用 Rust 程序，以兼容 Windows 下的 Grok 启动方式。

在修改插件之前，旧配置、注册记录、启动器、完整插件副本和已有后台任务定义备份到 `~/.grok/grok-budget/backups/<时间>/`。同名插件来自另一处仍存在的目录时停止，避免覆盖其他来源。历史继续使用 `~/.grok/grok-budget/history.sqlite3`，无需清空或迁移。

## 数据口径

| 指标 | 来源与含义 |
|---|---|
| 账户占比 | 固定官方接口 `https://cli-chat-proxy.grok.com/v1/billing?format=credits` 的 `config.creditUsagePercent`，1.0 = 1% |
| 周期、重置 | `config.currentPeriod.start/end`，不假设自然周或周一重置 |
| Build / Chat 占比 | `config.productUsage`，缺省值保持未知 |
| 本机成本 | 周期内已结束轮次的 `costUsdTicks / 10^10`，不是订阅实际扣款 |
| 累计 Token | 去重后轮次的 `totalTokens`；输入已含缓存读取、输出已含推理，不重复相加 |
| 会话回退 | 当前会话 `session.costUsdTicks` 与 `session.totalTokens`，包含 resume/fork 继承历史 |
| 美元周限 | 接口未公开，只能按记录成本与 Build 占比条件估算 |
| 周限 Token | 预估美元周限 × 同次采样的本周期 Token ÷ 本周期成本；受模型、缓存比例影响 |

同账户、同周期、无占比与成本回退，Build 占比增加至少 3 个百分点，样本间隔至少 60 秒时：

`周限金额 ≈ 成本增量 ÷ Build 占比增量 × 100`

样本不足时低可信度粗估：`周限金额 ≈ 本周期本机成本 ÷ Build 占比 × 100`。

优先使用 Build 产品占比；产品占比是否代表共享周池贡献没有官方换算说明。缺少 Build 占比且已有 Chat 消耗、0%、100% 封顶、非周周期、缺少成本或账本不完整时不估算。

敏感区间假设每个百分比端点误差 ±1 个百分点，单样本分母 ±1，双样本差值分母 ±2。这不是统计置信区间，不保证包含真实值；1% 单样本无法约束上界。

耗尽时间外推使用账户总占比（包括 Chat），要求至少 30 分钟的单调样本跨度和至少 1 个百分点变化，不再把 Build 达到 100% 当作共享额度耗尽。账本按轮次事实去重，避免 fork/resume 重复累计；按结束时间归属周期。其他设备、Chat、进行中调用、动态折扣与账户切换会影响结果。本地账本没有账户归属，历史记录可能混入其他账户。

## 安全与故障

令牌仅在内存中用于固定官方 HTTPS 接口，拒绝重定向，不打印、不入库、不自行刷新。SQLite 仅保留账户哈希、额度和汇总信息，保留 90 天历史，无聊天内容或凭据。

同一账户的状态栏、hooks 和手动查询共享短事务预约；强制刷新也不重复发起进行中的请求。正常刷新约 10/60 秒，保留 0.5 秒调度容差；自动请求最低间隔 5 秒。短暂失败按 10、20、40 秒逐步退避，最高 5 分钟；401/403 自动等待 5 分钟，重新登录后可手动 `--refresh`；429 遵守 Retry-After（支持秒数和 HTTP 日期），手动刷新也不能绕过限流。

写锁只覆盖短暂预约与读写，HTTP 和账本扫描不持有写锁；网络最多等待 5 秒。查询失败使用带旧数据标记的快照；额度不可用时仍尽量保留会话金额和 Token。hooks 静默失败，不阻断会话。

令牌过期时打开 Grok 刷新登录，必要时运行 `grok login`。看板是静态快照，浏览器刷新不重新查询，请重新运行 `看板.cmd`。

## 开发与验证

```powershell
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
cargo build --release --locked
./install.ps1 -Build
```

`src/data.rs` 负责接口、账本和估算，`src/storage.rs` 负责兼容旧数据库的采样缓存，`src/display.rs` 负责会话回退与显示。测试覆盖账户/周期隔离、去重、敏感区间、失败缓存、无网络长锁、恢复会话、累计即时更新、宽度和 HTML 转义。

## 卸载

运行 `./uninstall.ps1`：停用并删除本插件创建的 `GrokBudgetMonitor` 任务，只移除属于本插件的状态栏配置，再通过 Grok 官方命令卸载插件。历史数据库、运行文件和备份保留；重新启动 Grok 后生效。不要用整份旧配置覆盖后来新增的设置。

## 0.2.5 更新

- 修复状态栏数字跨字段匹配导致的串色和百分比警示颜色失效。
- 耗尽时间按账户总占比计算，计入 Chat 消耗。
- 独立后台刷新、活跃/空闲调度、结束后补查、跨会话请求合并和失败退避。
- 本地账本按文件变化复用聚合结果，减少重复扫描；显示额度样本年龄。
- 安装前完整备份，修复 Grok 本地插件注册版本不同步，提供后台任务卸载。
