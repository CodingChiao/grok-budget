# Grok Budget

Grok Build 的非官方额度监测插件。在状态栏中查看账户用量、重置时间、会话成本和 Token，也可以导出中文 HTML 看板。

**Windows x64 · Rust 原生 · 0.2.6 · MIT License**

[下载最新版](https://github.com/CodingChiao/grok-budget/releases/latest) · [版本记录](https://github.com/CodingChiao/grok-budget/releases) · [反馈问题](https://github.com/CodingChiao/grok-budget/issues)

## 功能

- **账户额度**：服务端已用百分比、Build / Chat 占比和实际重置时间。
- **会话统计**：累计成本和 Token，支持恢复会话时按准确的会话 ID 读取账本。
- **自动刷新**：活跃时约 10 秒查询账户，回答结束后主动查询并按需补查。
- **本地看板**：每日成本、模型消耗、数据质量、明暗主题及摘要导出。
- **历史估算**：记录额度样本，估算周限与耗尽时间，并标明未知、旧数据和不完整统计。

预编译分发包无需 Python、Node.js 或 Rust。独立查询和后台监测不调用模型；通过 `/budget` 让 Grok 解释结果可能消耗模型额度。

> 账户百分比来自服务端；美元和 Token 周限是本地估算，不是官方上限。本机成本是账本记录值，不等于订阅实际扣款。

## 快速开始

### 环境要求

- Windows x64，已安装并登录 Grok Build。
- 已验证 **Grok Build 1.0.41**；其他版本尚未验证。
- 自动刷新需要系统允许创建当前用户的 Windows 计划任务。

### 下载与安装

从 [Releases](https://github.com/CodingChiao/grok-budget/releases/latest) 下载 Windows x64 ZIP，解压后在该目录打开 PowerShell：

```powershell
./install.ps1
```

安装器会注册插件、配置状态栏，并启动当前用户后台任务 `GrokBudgetMonitor`。任务使用普通用户权限、隐藏运行，不保存登录密码。

**安装或更新后，请重新启动 Grok 客户端，让定时配置和 hooks 生效。** 安装器不会主动结束当前会话。

发布页提供 SHA-256 校验文件，可用 `Get-FileHash <下载的ZIP路径> -Algorithm SHA256` 核对下载文件。

## 使用

在分发包根目录直接调用原生程序：

```powershell
# 查询额度与本机统计
./grok-budget/grok-budget.exe

# 主动刷新服务端数据
./grok-budget/grok-budget.exe --refresh

# 输出 JSON 摘要
./grok-budget/grok-budget.exe --json

# 只读历史快照，不联网、不刷新本机统计
./grok-budget/grok-budget.exe --offline

# 持续采样；Ctrl+C 停止
./grok-budget/grok-budget.exe --watch 10

# 生成看板并用默认浏览器打开
./grok-budget/grok-budget.exe --html output/grok-budget.html
Start-Process ./output/grok-budget.html
```

新 Grok 会话中的 `/budget` 可以让模型解释查询结果。看板是静态快照，刷新浏览器不会重新查询；需要新数据时，再执行一次 `--html` 导出命令。

`--no-color` 或环境变量 `NO_COLOR` 可关闭颜色。默认读取当前用户的 `.grok` 目录；`GROK_HOME` 或 `--grok-home` 可指定其他目录，`--data-dir` 可指定插件数据目录。独立查询可直接运行，自动后台刷新需要先安装。

## 状态栏怎么看

示意数据：

```text
会话 $0.53 · 1.64M Token        已用 $1.54 · 5.94M Token · ░░░░░░░░░░ 4%
周限 ≈$51.19 · ≈197.89M Token   重置 10-06 00:08 · 更新于 3秒前
```

| 字段 | 含义 |
| --- | --- |
| 会话 | 当前会话累计成本和输入＋输出 Token |
| 已用 | 本周期本机成本、Token，以及服务端账户已用百分比 |
| 周限 | 美元和 Token 预估上限，使用 `≈` 标记 |
| 重置 | 服务端周期结束时间，按本机时区显示 |
| 更新于 | 账户额度样本年龄，不代表服务端记账延迟 |

金额为青色，Token 为品红色；账户已用达到 80% / 95% 时，状态栏以黄色 / 红色提示。窗口变窄时省略进度条或改为最多四行。

`$--` / `--` 表示未知，`*` 表示统计不完整，“旧数据”表示查询失败、样本超过 180 秒或周期已结束。额度暂不可用时，仍尽量显示当前会话统计。

## 刷新机制

状态栏和 hooks 只提交信号，由独立后台任务统一查询。同一账户的多个会话共用缓存与请求预约。

| 场景 | 行为 |
| --- | --- |
| 状态栏显示 | 每 2 秒读取本地结果；会话状态变化也会触发更新 |
| 活跃会话 | 账户额度约每 10 秒查询一次 |
| 空闲会话 | 账户额度约每 60 秒查询一次 |
| 会话启动 / 恢复 | 提交立即查询信号，受请求合并和最低间隔限制 |
| 回答结束 | 提交约 2 秒后的查询；额度信息未变化时，约 10 秒后补查一次 |
| 无会话信号 | 最后一次状态栏 / hook 信号超过 20 秒后，不再自动访问额度接口 |
| 手动刷新 | 跳过普通缓存等待，不重复发起进行中的请求，也不绕过服务端限流 |

自动请求至少间隔 5 秒，刚完成的查询可能合并或推迟事件请求。最后一次活跃信号后保留 60 秒活跃窗口；实际显示延迟还受服务端记账和状态栏刷新时机影响。

本机统计按账本文件变化刷新，聚合缓存最多保留 10 秒。新显示统计不会写回历史估算样本：JSON 的 `local` 为额度采样时的统计，`live_local` 为本次显示统计，`local_refreshed_at` 为显示统计更新时间。

网络请求最多等待 5 秒。普通失败按 10、20、40 秒逐步退避，最高 5 分钟；401/403 自动等待 5 分钟，重新登录后可手动刷新；429 遵守 `Retry-After`。

## 数据来源与估算

| 数据 | 来源与范围 |
| --- | --- |
| 账户占比 | 官方 `https://cli-chat-proxy.grok.com/v1/billing?format=credits` 接口；`creditUsagePercent` 的 1.0 表示 1% |
| 周期与重置 | `currentPeriod.start/end`，不假设自然周或周一重置 |
| Build / Chat 占比 | `productUsage`，接口未返回时保持未知 |
| 本机成本 | 周期内已结束轮次的 `costUsdTicks / 10^10` |
| 累计 Token | 去重后的 `totalTokens`，不重复加上已包含的缓存或推理 Token |
| 周限 | 根据成本与占比推算，接口未公开美元或 Token 周上限 |

账本按轮次事实去重、按结束时间归入周期。其他设备、Chat、进行中的调用和动态折扣会造成偏差；账本没有账户归属，切换账户后可能混入旧账户记录。

<details>
<summary>估算公式与适用条件</summary>

优先使用 Build 产品占比。同账户、同周期、无占比和成本回退，占比增量至少 3 个百分点、样本间隔至少 60 秒时：

```text
预估美元周限 = 成本增量 ÷ Build 占比增量 × 100
```

样本不足时使用低可信度粗估：

```text
预估美元周限 = 本周期本机成本 ÷ Build 占比 × 100
预估 Token 周限 = 预估美元周限 × 同次采样 Token ÷ 同次采样成本
```

缺少可用占比、0% 或 100% 封顶、非周周期、成本缺失、账本不完整时不估算。缺少 Build 占比且已有 Chat 消耗时，不直接用账户总占比反推美元周限。产品占比与共享周池的换算没有官方说明，估算依赖本机消耗覆盖率和计费权重稳定的假设。

敏感区间假设每个百分比端点存在 ±1 个百分点误差；它不是统计置信区间，也不保证覆盖真实上限。1% 单样本无法约束上界。

耗尽时间使用账户总占比（包含 Chat），要求至少 30 分钟的单调样本跨度和至少 1 个百分点变化；它只是按历史增速外推。

</details>

## 更新与卸载

下载新版本后运行 `./install.ps1`。若同名插件来自另一个仍存在的目录，安装器会停止；使用原安装来源目录更新即可。

安装前会将旧配置、插件副本、注册信息及已有任务定义备份到 `.grok/grok-budget/backups/<时间>/`。历史数据库位于 `.grok/grok-budget/history.sqlite3`，更新时保留数据。

安装器生成的 `.grok/grok-budget/statusline.cmd` 是 Grok 的兼容启动入口，需要保留。仓库和分发包不再提供额外的 `.cmd` 包装脚本。

```powershell
./uninstall.ps1
```

卸载脚本会停止并删除本插件的后台任务，移除属于本插件的状态栏配置，再通过 Grok 卸载插件。历史、运行文件和备份保留；重启 Grok 后生效。

## 常见问题

**安装后状态栏没变化？**

重新启动 Grok 客户端。Grok 在启动时读取配置，已经打开的客户端可能继续使用旧定时设置。

**一直显示旧数据？**

先运行 `./grok-budget/grok-budget.exe --refresh` 查看错误。登录失效时打开 Grok 刷新登录，必要时执行 `grok login`。后台状态可通过以下命令查看：

```powershell
Get-ScheduledTask -TaskName GrokBudgetMonitor | Select-Object TaskName, State
```

**为什么金额变了，百分比没变？**

金额来自本机账本，百分比来自服务端，两者的更新时机与统计范围不同。服务端记账也可能延迟。

**会上传聊天内容吗？**

插件不读取聊天记录。登录令牌仅在内存中用于固定官方 HTTPS 接口，拒绝重定向，不打印、不入库、不自行刷新。SQLite 保存账户哈希、额度、汇总信息和刷新调度状态，额度样本保留 90 天。HTML 看板没有外部资源或追踪。

## 从源码构建

需要 Rust 工具链和 Windows C++ 构建工具。在仓库根目录执行：

```powershell
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
./install.ps1 -Build
```

仅编译时执行 `cargo build --release --locked`，程序位于 `target/release/grok-budget.exe`。

| 路径 | 内容 |
| --- | --- |
| `src/` | 额度接口、账本统计、缓存调度和终端显示 |
| `grok-budget/` | 插件清单、hooks、`/budget` 命令及内嵌看板模板 |
| `install.ps1` / `uninstall.ps1` | 安装、更新与卸载 |
| `tests/native.rs` | 数据口径、刷新调度、并发与显示回归测试 |

本仓库只包含 Grok Budget 插件，不包含 Grok Build 本体、账户凭据或使用历史。使用 [MIT License](LICENSE) 发布。
