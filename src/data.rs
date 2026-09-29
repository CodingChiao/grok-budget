use crate::*;
use anyhow::{Result, bail};
use chrono::{DateTime, Local};
use serde_json::{Map, Value, json};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

pub fn normalize_billing(data: &Value) -> Result<Value> {
    let c = &data["config"];
    let p = &c["currentPeriod"];
    if timestamp(&p["end"])? <= timestamp(&p["start"])? {
        bail!("额度周期起止无效。");
    }
    let used = number(&c["creditUsagePercent"])
        .filter(|p| *p <= 100.0)
        .context("额度接口缺少有效的 0–100 百分比")?;
    let products: Vec<Value> = c["productUsage"].as_array().into_iter().flatten()
        .filter(|v| matches!(s(&v["product"]), "GrokBuild" | "GrokChat"))
        .map(|v| json!({"product":v["product"], "used_percent":number(&v["usagePercent"]).filter(|p| *p <= 100.0)})).collect();
    Ok(
        json!({"period_type":p["type"].as_str().unwrap_or("UNKNOWN"), "period_start":p["start"], "period_end":p["end"],
        "used_percent":used, "remaining_percent":100.0-used, "unified_billing":c["isUnifiedBillingUser"]==true,
        "products":products, "official_limit_usd":null}),
    )
}

#[derive(Debug)]
pub struct BillingFailure {
    pub message: &'static str,
    pub retry_seconds: f64,
    pub rate_limited: bool,
}
impl std::fmt::Display for BillingFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for BillingFailure {}
pub fn retry_after(value: Option<&str>, at: f64) -> f64 {
    value
        .and_then(|v| {
            v.trim()
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .or_else(|| {
                    DateTime::parse_from_rfc2822(v)
                        .ok()
                        .map(|d| d.timestamp() as f64 - at)
                })
        })
        .unwrap_or(60.0)
        .max(5.0)
}

pub fn fetch_billing(key: &str) -> Result<Value> {
    let started = std::time::Instant::now();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("无法初始化额度连接")?;
    let response = client
        .get(ENDPOINT)
        .bearer_auth(key)
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            concat!("grok-budget/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .map_err(|_| anyhow::anyhow!("额度接口连接失败或超时。"))?;
    let status = response.status();
    if status.is_redirection() {
        bail!("额度接口发生重定向，已停止请求。");
    }
    if matches!(status.as_u16(), 401 | 403) {
        return Err(BillingFailure {
            message: "Grok 登录已失效或无权访问额度，请打开 Grok 刷新登录。",
            retry_seconds: 300.0,
            rate_limited: false,
        }
        .into());
    }
    if status.as_u16() == 429 {
        return Err(BillingFailure {
            message: "额度接口限流，已按 Retry-After 延后查询。",
            rate_limited: true,
            retry_seconds: retry_after(
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok()),
                now(),
            ) + started.elapsed().as_secs_f64(),
        }
        .into());
    }
    if !status.is_success() {
        bail!("额度接口暂不可用（HTTP {}）。", status.as_u16());
    }
    let mut raw = Vec::new();
    response
        .take(1_000_001)
        .read_to_end(&mut raw)
        .context("额度响应读取失败")?;
    if raw.len() > 1_000_000 {
        bail!("额度接口返回内容过大。");
    }
    normalize_billing(&serde_json::from_slice(&raw).context("额度响应不是有效 JSON")?)
}

pub fn ledgers(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(workspaces) = fs::read_dir(home.join("sessions")) {
        for ws in workspaces.flatten() {
            if let Ok(sessions) = fs::read_dir(ws.path()) {
                for session in sessions.flatten() {
                    let p = session.path().join("usage.json");
                    if p.is_file() {
                        paths.push(p);
                    }
                }
            }
        }
    }
    paths.sort();
    paths
}

fn add(v: &mut Value, field: &str, value: f64) {
    v[field] = json!(n(&v[field]) + value);
}
fn partial(v: &Value) -> bool {
    number(&v["costUsdTicks"]).is_none()
        || v["costIsPartial"] == true
        || n(&v["costMissingCalls"]) > 0.0
}
pub fn local_usage(home: &Path, start: f64, end: f64) -> Value {
    let mut u = json!({"cost_usd":0,"cost_ticks":0,"turns":0,"sessions":0,"duplicates":0,"unreadable_files":0,
        "undated_turns":0,"partial_cost_turns":0,"ledger_files":0,"models":{},"daily":{}});
    for f in FIELDS {
        u[f] = json!(0);
    }
    let mut seen = HashSet::new();
    let mut sessions = HashSet::new();
    for path in ledgers(home) {
        add(&mut u, "ledger_files", 1.0);
        let ledger = match read_json(&path) {
            Ok(v) if v["turns"].is_array() => v,
            _ => {
                add(&mut u, "unreadable_files", 1.0);
                continue;
            }
        };
        for turn in ledger["turns"].as_array().unwrap() {
            let Ok(ended_at) = parse_time(&turn["endedAt"]) else {
                add(&mut u, "undated_turns", 1.0);
                continue;
            };
            let ended = seconds(&ended_at);
            if ended < start || ended >= end {
                continue;
            }
            let facts: Map<String, Value> = FIELDS
                .iter()
                .copied()
                .chain([
                    "endedAt",
                    "costUsdTicks",
                    "costIsPartial",
                    "costMissingCalls",
                    "primaryModelId",
                    "modelUsage",
                ])
                .map(|key| (key.to_string(), turn[key].clone()))
                .collect();
            if !seen.insert(Value::Object(facts).to_string()) {
                add(&mut u, "duplicates", 1.0);
                continue;
            }
            sessions.insert(
                ledger["sessionId"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| path.parent().unwrap().to_string_lossy().into()),
            );
            add(&mut u, "turns", 1.0);
            if partial(turn) {
                add(&mut u, "partial_cost_turns", 1.0);
            }
            add(&mut u, "cost_ticks", n(&turn["costUsdTicks"]));
            for f in FIELDS {
                add(&mut u, f, n(&turn[f]));
            }
            let day = ended_at
                .with_timezone(&Local)
                .format("%Y-%m-%d")
                .to_string();
            add(&mut u["daily"], &day, n(&turn["costUsdTicks"]) / TICKS);
            // Per-model rows come from modelUsage; a ledger without one bills the
            // whole turn to its primary model.
            let models: Vec<(&str, &Value)> = match turn["modelUsage"].as_object() {
                Some(m) if !m.is_empty() => m.iter().map(|(k, v)| (k.as_str(), v)).collect(),
                _ => vec![(turn["primaryModelId"].as_str().unwrap_or("unknown"), turn)],
            };
            for (model, values) in models {
                if !values.is_object() {
                    continue;
                }
                if u["models"][model].is_null() {
                    u["models"][model] = json!({"cost_usd":0,"input_tokens":0,"cached_tokens":0,"output_tokens":0,"calls":0,"partial":false});
                }
                let row = &mut u["models"][model];
                add(row, "cost_usd", n(&values["costUsdTicks"]) / TICKS);
                row["partial"] = json!(row["partial"] == true || partial(values));
                for (source, dest) in [
                    ("inputTokens", "input_tokens"),
                    ("cachedReadTokens", "cached_tokens"),
                    ("outputTokens", "output_tokens"),
                    ("modelCalls", "calls"),
                ] {
                    add(row, dest, n(&values[source]));
                }
            }
        }
    }
    u["cost_usd"] = json!(n(&u["cost_ticks"]) / TICKS);
    u["sessions"] = json!(sessions.len());
    u
}

pub fn estimate_percentage(q: &Value) -> (Option<f64>, &'static str) {
    let products = q["products"].as_array();
    if let Some(v) = products
        .into_iter()
        .flatten()
        .find(|v| v["product"] == "GrokBuild")
    {
        return (number(&v["used_percent"]), "GrokBuild 产品占比");
    }
    if products
        .into_iter()
        .flatten()
        .any(|v| v["product"] != "GrokBuild" && n(&v["used_percent"]) > 0.0)
    {
        return (None, "产品占比缺失");
    }
    (
        number(&q["used_percent"]),
        "账户总占比（假设全部由 Build 消耗）",
    )
}
fn complete(u: &Value) -> bool {
    ["unreadable_files", "undated_turns", "partial_cost_turns"]
        .iter()
        .all(|k| n(&u[k]) == 0.0)
}
fn same_period(a: &Value, b: &Value) -> bool {
    a["account"] == b["account"]
        && a["quota"]["period_start"] == b["quota"]["period_start"]
        && a["quota"]["period_end"] == b["quota"]["period_end"]
}
fn unavailable(reason: &str) -> Value {
    json!({"available":false,"reason":reason,"summary":reason})
}
pub fn estimate(current: &Value, history: &[Value]) -> Value {
    let q = &current["quota"];
    let u = &current["local"];
    let (pct, basis) = estimate_percentage(q);
    let cost = n(&u["cost_usd"]);
    if !s(&q["period_type"]).contains("WEEKLY") {
        return unavailable("当前账户不是周周期，不能解释为周限。");
    }
    if !complete(u) {
        return unavailable("本地账本不完整，暂不推算美元周限。");
    }
    let Some(pct) = pct else {
        return unavailable("缺少可用的 Build 百分比，不能用包含 Chat 的总占比反推。");
    };
    if pct <= 0.0 || pct >= 100.0 || n(&q["used_percent"]) >= 100.0 || cost <= 0.0 {
        return unavailable("需要非零成本且用量介于 0% 和 100% 之间；封顶百分比不能反推。");
    }
    let (mut last_pct, mut last_cost) = (pct, cost);
    let mut candidate: Option<(f64, f64)> = None;
    for old in history.iter().rev() {
        if !same_period(current, old) {
            continue;
        }
        let (op, obasis) = estimate_percentage(&old["quota"]);
        let Some(op) = op else {
            break;
        };
        // A missing cost is not zero. Treating it as zero invents a delta.
        let Some(oc) = number(&old["local"]["cost_usd"]) else {
            break;
        };
        if obasis != basis || op > last_pct || oc > last_cost {
            break;
        }
        (last_pct, last_cost) = (op, oc);
        let (dp, dc) = (pct - op, cost - oc);
        if dp >= 3.0
            && dc > 0.0
            && n(&current["sampled_at"]) - n(&old["sampled_at"]) >= 60.0
            && complete(&old["local"])
            && candidate.is_none_or(|v| (dp, dc) > v)
        {
            candidate = Some((dp, dc));
        }
    }
    if let Some((dp, dc)) = candidate {
        return json!({"available":true,"method":"delta","basis":basis,"confidence":"参考估算","usd":dc*100.0/dp,
            "low_usd":dc*100.0/(dp+2.0),"high_usd":dc*100.0/(dp-2.0),"delta_percent":dp,"delta_cost_usd":dc,
            "reason":format!("同周期成本增量 ÷ {basis}增量 × 100；假设本机覆盖全部 Build 消耗且计费权重稳定。")});
    }
    json!({"available":true,"method":"period","basis":basis,"confidence":"粗估 / 低可信度","usd":cost*100.0/pct,
        "low_usd":cost*100.0/(pct+1.0),"high_usd":if pct>1.0 {Some(cost*100.0/(pct-1.0))} else {None},
        "reason":format!("本周期本机成本 ÷ {basis} × 100；尚无 ≥3 个百分点的增量样本，不是官方上限。")})
}

pub fn duration(seconds: f64) -> String {
    let mins = (seconds.max(0.0) / 60.0) as u64;
    if mins >= 1440 {
        format!("{} 天 {} 小时", mins / 1440, mins % 1440 / 60)
    } else if mins >= 60 {
        format!("{} 小时 {} 分", mins / 60, mins % 60)
    } else {
        format!("{mins} 分")
    }
}
pub fn extrapolate(current: &Value, history: &[Value]) -> Value {
    let q = &current["quota"];
    let basis = "账户总占比";
    if !s(&q["period_type"]).contains("WEEKLY") {
        return unavailable("当前不是周周期，不外推耗尽时间。");
    }
    let Some(pct) = number(&q["used_percent"]).filter(|p| *p > 0.0 && *p < 100.0) else {
        return unavailable("占比缺失、为零或已封顶，不外推耗尽时间。");
    };
    let (mut last_pct, mut last_cost) = (pct, n(&current["local"]["cost_usd"]));
    let mut anchor = None;
    for old in history.iter().rev() {
        if !same_period(current, old) {
            continue;
        }
        let Some((op, oc)) =
            number(&old["quota"]["used_percent"]).zip(number(&old["local"]["cost_usd"]))
        else {
            break;
        };
        if op > last_pct || oc > last_cost {
            break;
        }
        anchor = Some((op, n(&old["sampled_at"])));
        (last_pct, last_cost) = (op, oc);
    }
    let Some((ap, at)) = anchor else {
        return unavailable("还没有更早的同周期样本，不外推耗尽时间。");
    };
    let span = n(&current["sampled_at"]) - at;
    let dp = pct - ap;
    if span < 1800.0 || dp < 1.0 {
        return unavailable("样本跨度不足 30 分钟或占比变化不足 1 个百分点，不外推耗尽时间。");
    }
    let hours = span / 3600.0;
    let rate = dp / hours;
    let left = (timestamp(&q["period_end"]).unwrap_or(0.0) - n(&current["sampled_at"])).max(0.0);
    let hours_to_full = (100.0 - pct) / rate;
    let projected = pct + rate * left / 3600.0;
    let prediction = if hours_to_full * 3600.0 <= left {
        format!("约 {}后用满", duration(hours_to_full * 3600.0))
    } else {
        format!("到重置约 {:.0}%", projected.min(100.0))
    };
    let summary = format!(
        "近 {hours:.1} 小时内，{basis}约 +{rate:.2} 个百分点/小时；照此{prediction}（外推，不是官方额度）"
    );
    json!({"available":true,"basis":basis,"percent_per_hour":rate,"window_hours":hours,"delta_percent":dp,
        "projected_used_percent":projected,"hours_to_full":hours_to_full,"reason":summary,"summary":summary})
}
