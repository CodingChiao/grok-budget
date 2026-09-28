# Grok Budget 0.2.0

Grok Build 的 Rust 原生额度插件，适配 Windows 上的 Grok Build 1.0.41。读取账户真实占比、重置时间，汇总本机会话与本周期成本和 Token，并持续记录样本、估算周限。

运行、安装与看板导出均不需要 Python、Node 或 Rust；从源码编译才需要 Rust。HTTP、SQLite 与 HTML 模板包含在 `grok-budget/grok-budget.exe` 中。

## 使用

```powershell
./grok-budget.cmd                         # 查询
./grok-budget.cmd --refresh               # 立即刷新服务端
./grok-budget.cmd --json                  # 结构化摘要
./grok-budget.cmd --offline               # 完整历史快照，不联网、不刷新本地统计
./grok-budget.cmd --watch 60              # 持续采样，Ctrl+C 停止
./grok-budget.cmd --html output/grok-budget.html
```

双击 `看板.cmd` 生成并打开中文看板。新会话中的 `/budget` 让 Grok 调用插件解释结果，模型解释可能消耗额度；独立查询不调用模型。

## 状态栏

正常前景色、紧凑两列，不使用暗淡文字、竖线分隔或随窗口拉大的间距。会话金额与 Token 同行；金额用青色强调，已用占比带字符进度条并按 80% / 95% 阈值切换绿、黄、红。窗口过窄时省略进度条，更窄时退化为单行列表。示例：

```text
会话 $0.53 · 1.64M Token        已用 $1.54 · 5.94M Token · ░░░░░░░░░░ 4%
周限 ≈$51.19 · ≈197.89M Token   重置 10-06 00:08
```

- 会话：优先使用 Grok 传入的金额和累计输入＋输出 Token。金额缺失时，按准确的 `session_id` 成对读取对应账本的金额和 Token，避免恢复会话传入的 0 覆盖真实累计值。不猜测最新会话，不读取聊天记录。
- 已用：本周期本机累计金额、累计 Token、服务端账户已用占比。百分比可能包含其他设备和 Chat；金额和 Token 只覆盖本机已记录轮次。
- 周限：预估美元周限和预估 Token，以 `≈` 标记，均不是官方上限。
- 重置：账户周期实际结束时间，按本机时区显示；旧快照明确标记。

未知金额显示 `$--`，未知 Token 显示 `--`，零值保留。`*` 表示成本或账本不完整。80% 起黄色提示，95% 起红色提示；`--no-color` 或 `NO_COLOR` 关闭颜色。窄窗口分为最多四行。

**本地累计金额和 Token 每次状态栏刷新都重新读取账本，不等待 60 秒额度缓存。** 进行中的调用仍需 Grok 写入账本才可计入。账户占比每 60 秒查询，SessionStart / Stop hooks 也采样。

JSON 的 `local` 为与服务端额度同次采样的历史统计，`live_local` 为本次新读取的显示统计，`local_refreshed_at` 为更新时间。显示数据不写回历史样本，不用新成本搭配旧百分比重新估算。`--offline` 只返回历史快照。

## 安装与更新

```powershell
./install.ps1                 # 使用分发包中的 Windows x64 程序安装
./install.ps1 -Build          # 从源码编译后安装
```

首次启用状态栏需要重启 Grok。已经启用时，更新现有启动器后下次刷新即可使用 Rust，不结束当前会话。新 hooks 和 `/budget` 在 Grok 重新加载插件后生效。

安装器通过 Grok 的 `plugin validate/install/update/enable` 注册插件，并明确同步安装副本。状态栏使用 `~/.grok/grok-budget/statusline.cmd` 调用 Rust 程序，以兼容 Windows 下的 Grok 启动方式。

旧配置、启动器和已有运行文件备份到 `~/.grok/grok-budget/backups/<时间>/`。同名插件来自另一处仍存在的目录时停止，避免覆盖其他来源。历史继续使用 `~/.grok/grok-budget/history.sqlite3`，无需清空或迁移。

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

耗尽时间外推要求至少 30 分钟的单调样本跨度和至少 1 个百分点变化。账本按轮次事实去重，避免 fork/resume 重复累计；按结束时间归属周期。其他设备、Chat、进行中调用、动态折扣与账户切换会影响结果。本地账本没有账户归属，历史记录可能混入其他账户。

## 安全与故障

令牌仅在内存中用于固定官方 HTTPS 接口，拒绝重定向，不打印、不入库、不自行刷新。SQLite 仅保留账户哈希、额度和汇总信息，保留 90 天历史，无聊天内容或凭据。

60 秒缓存合并重复采样。写锁只覆盖短暂预约与读写，HTTP 和账本扫描不持有写锁；网络最多等待 5 秒。查询失败使用带旧数据标记的快照；额度不可用时仍尽量保留会话金额和 Token。hooks 静默失败，不阻断会话。

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

删除 `~/.grok/config.toml` 中本插件的 `[ui.status_line]` 配置，或仅恢复对应的旧状态栏配置，再执行 `grok plugin uninstall grok-budget --confirm --keep-data`。历史数据保留，重启 Grok 后停用。不要用整份旧配置覆盖后来新增的设置。
