use crate::*;
use serde_json::Value;
use std::{fs, path::Path};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Default)]
pub struct Session {
    pub cost: Option<f64>,
    pub tokens: Option<f64>,
    pub partial: bool,
}

fn sum_fields(value: &Value, fields: &[&str]) -> Option<f64> {
    fields
        .iter()
        .map(|f| number(&value[*f]))
        .collect::<Option<Vec<_>>>()
        .map(|v| v.iter().sum())
}
pub fn session_usage(home: &Path, payload: &Value) -> Session {
    let context = &payload["context_window"];
    let mut session = Session {
        cost: number(&payload["cost"]["total_cost_usd"]),
        tokens: sum_fields(context, &["session_input_tokens", "session_output_tokens"]).or_else(
            || {
                sum_fields(
                    &context["session_usage"],
                    &[
                        "input_tokens",
                        "output_tokens",
                        "cache_creation_input_tokens",
                        "cache_read_input_tokens",
                    ],
                )
            },
        ),
        partial: false,
    };
    if session.cost.is_some() && session.tokens.is_some() {
        return session;
    }
    let id = s(&payload["session_id"]);
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return session;
    }
    // Match the exact session; never guess from the newest ledger or read chat logs.
    for path in data::ledgers(home) {
        if path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|v| v.to_str())
            != Some(id)
        {
            continue;
        }
        let Ok(ledger) = read_json(&path) else {
            continue;
        };
        if s(&ledger["sessionId"]) != id {
            continue;
        }
        let totals = &ledger["session"];
        let fallback_cost = session.cost.is_none();
        if fallback_cost {
            session.cost = number(&totals["costUsdTicks"]).map(|v| v / TICKS);
            session.partial =
                totals["costIsPartial"] == true || n(&totals["costMissingCalls"]) > 0.0;
        }
        if session.tokens.is_none() || (fallback_cost && session.cost.is_some()) {
            // On resume Grok can omit cost and send zero process-local tokens.
            // When recovering ledger cost, recover the matching ledger tokens too.
            session.tokens = number(&totals["totalTokens"])
                .or_else(|| sum_fields(totals, &["inputTokens", "outputTokens"]))
                .or(session.tokens);
        }
        break;
    }
    session
}
pub fn money(value: Option<f64>) -> String {
    match value {
        Some(v) if v > 0.0 && v < 0.01 => format!("${v:.4}"),
        Some(v) => format!("${v:.2}"),
        None => "$--".into(),
    }
}
pub fn tokens(value: Option<f64>) -> String {
    let Some(value) = value else {
        return "--".into();
    };
    for (scale, suffix) in [(1e9, "B"), (1e6, "M"), (1e3, "K")] {
        if value >= scale {
            return format!(
                "{}{suffix}",
                format!("{:.2}", value / scale)
                    .trim_end_matches('0')
                    .trim_end_matches('.')
            );
        }
    }
    format!("{value:.0}")
}
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}
fn clip(text: &str, columns: usize) -> String {
    if width(text) <= columns {
        return text.into();
    }
    let mut out = String::new();
    for c in text.chars() {
        if width(&out) + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
            > columns.saturating_sub(1)
        {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}
fn usage_bar(used: f64) -> String {
    let filled = ((used.clamp(0.0, 100.0) / 10.0).round() as usize).min(10);
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}
pub fn status_text(
    report: Option<&Value>,
    session: &Session,
    columns: usize,
    color: bool,
) -> String {
    let columns = columns.clamp(1, 500);
    // No dim, faint, custom font or letter spacing. Labels keep the terminal foreground;
    // accents stay limited to 36 for money, 35 for token counts, and 31/32/33 for the
    // usage bar and percent. Colors are injected after layout, so padding and clipping
    // only measure visible text.
    let sep = " · ";
    let session_money = money(session.cost);
    let session_tokens = tokens(session.tokens);
    let current = format!(
        "会话 {session_money}{}{sep}{session_tokens} Token",
        if session.partial { "*" } else { "" },
    );
    let mut spent_money = String::new();
    let mut spent_tokens = String::new();
    let mut limit_money = String::new();
    let mut weekly_tokens = String::new();
    let mut used: Option<f64> = None;
    let mut pct = String::new();
    let mut bar: Option<String> = None;
    let mut bar_in_spent = false;
    let output;
    if let Some(r) = report {
        let usage = display_usage(r);
        let limit = if r["estimate"]["available"] == true {
            number(&r["estimate"]["usd"])
        } else {
            None
        };
        let token_limit = limit
            .zip(number(&r["local"]["totalTokens"]))
            .zip(number(&r["local"]["cost_usd"]))
            .filter(|((l, t), c)| *l > 0.0 && *t > 0.0 && *c > 0.0)
            .map(|((l, t), c)| t * l / c);
        used = number(&r["quota"]["used_percent"]);
        pct = used
            .map(|v| format!("{v}%"))
            .unwrap_or_else(|| "--%".into());
        bar = used.map(usage_bar);
        let incomplete = ["unreadable_files", "undated_turns", "partial_cost_turns"]
            .iter()
            .any(|key| n(&usage[*key]) > 0.0);
        spent_money = money(number(&usage["cost_usd"]));
        spent_tokens = tokens(number(&usage["totalTokens"]));
        let spent_base = format!(
            "已用 {spent_money}{}{sep}{spent_tokens} Token",
            if incomplete { "*" } else { "" },
        );
        limit_money = format!("{}{}", if limit.is_some() { "≈" } else { "" }, money(limit));
        weekly_tokens = format!(
            "{}{}",
            if token_limit.is_some() { "≈" } else { "" },
            tokens(token_limit)
        );
        let weekly = format!("周限 {limit_money}{sep}{weekly_tokens} Token");
        let mut reset = format!(
            "重置 {}{}{}",
            local_date(&r["quota"]["period_end"], "%m-%d %H:%M"),
            if r["stale"] == true { sep } else { "" },
            if r["stale"] == true { "旧数据" } else { "" }
        );
        if let Some(age) = number(&r["cache_age_seconds"]) {
            reset.push_str(&format!("{sep}更新于 {}秒前", age as u64));
        }
        // Try the two-row grid with the usage bar, then without it, then fall back to
        // clipped single cells. A bounded gutter keeps the columns close at every width.
        let mut laid_out = String::new();
        for with_bar in [true, false] {
            let spent_cell = match (&bar, with_bar) {
                (Some(b), true) => format!("{spent_base}{sep}{b} {pct}"),
                _ => format!("{spent_base}{sep}{pct}"),
            };
            let left = width(&current).max(width(&weekly));
            let right = width(&spent_cell).max(width(&reset));
            if left + right + 4 <= columns {
                laid_out = format!(
                    "{}{}{}\n{}{}{}",
                    current,
                    " ".repeat(left - width(&current) + 4),
                    spent_cell,
                    weekly,
                    " ".repeat(left - width(&weekly) + 4),
                    reset
                );
                bar_in_spent = bar.is_some() && with_bar;
                break;
            }
        }
        if laid_out.is_empty() {
            let spent_cell = format!("{spent_base}{sep}{pct}");
            laid_out = [&current, &spent_cell, &weekly, &reset]
                .map(|line| clip(line, columns))
                .join("\n");
        }
        output = laid_out;
    } else {
        output = format!(
            "{}\n{}",
            clip(&current, columns),
            clip("额度暂不可用  请运行 grok-budget.cmd", columns)
        );
    }
    if !color {
        return output;
    }
    // Locate fields inside their own cells, never search numeric values across a row.
    let fields = [
        ("会话", session_money.as_str(), session_tokens.as_str()),
        ("已用", spent_money.as_str(), spent_tokens.as_str()),
        ("周限", limit_money.as_str(), weekly_tokens.as_str()),
    ];
    let mut painted = String::new();
    for (row, line) in output.lines().enumerate() {
        if row > 0 {
            painted.push('\n');
        }
        let mut ranges = Vec::new();
        for (label, amount, count) in fields {
            let Some(cell) = line.find(label) else {
                continue;
            };
            let start = cell + label.len() + 1;
            if line
                .get(start..)
                .is_some_and(|tail| tail.starts_with(amount))
                && !amount.is_empty()
            {
                ranges.push((start, start + amount.len(), 36));
            }
            let partial_mark = line
                .get(start + amount.len()..)
                .is_some_and(|tail| tail.starts_with('*'));
            let token_start = start + amount.len() + usize::from(partial_mark) + sep.len();
            if !count.is_empty()
                && count.trim_start_matches('≈') != "--"
                && line
                    .get(token_start..)
                    .is_some_and(|tail| tail.starts_with(count))
            {
                ranges.push((token_start, token_start + count.len(), 35));
            }
            if label == "已用"
                && let Some(u) = used
            {
                let span = match (&bar, bar_in_spent) {
                    (Some(b), true) => format!("{b} {pct}"),
                    _ => pct.clone(),
                };
                let usage_start = token_start + count.len() + " Token".len() + sep.len();
                if line
                    .get(usage_start..)
                    .is_some_and(|tail| tail.starts_with(&span))
                {
                    ranges.push((
                        usage_start,
                        usage_start + span.len(),
                        if u >= 95.0 {
                            31
                        } else if u >= 80.0 {
                            33
                        } else {
                            32
                        },
                    ));
                }
            }
        }
        ranges.sort_unstable_by_key(|r| r.0);
        let mut cursor = 0;
        for (start, end, code) in ranges {
            painted.push_str(&line[cursor..start]);
            painted.push_str(&format!("\x1b[{code}m{}\x1b[0m", &line[start..end]));
            cursor = end;
        }
        painted.push_str(&line[cursor..]);
    }
    painted
}
pub fn report_text(r: &Value) -> String {
    let q = &r["quota"];
    let u = display_usage(r);
    let e = &r["estimate"];
    let mut lines = vec![
        "Grok Build 用量 / 周额度".into(),
        format!("账户已用 {}%（服务端）", n(&q["used_percent"])),
    ];
    for p in q["products"].as_array().into_iter().flatten() {
        lines.push(format!(
            "{} 占比：{}",
            s(&p["product"]),
            number(&p["used_percent"])
                .map(|v| format!("{v}%"))
                .unwrap_or_else(|| "未知".into())
        ));
    }
    lines.push(format!(
        "周期：{} → {}",
        local_date(&q["period_start"], "%Y-%m-%d %H:%M:%S %Z"),
        local_date(&q["period_end"], "%Y-%m-%d %H:%M:%S %Z")
    ));
    lines.push(format!(
        "距离重置：{}",
        data::duration(timestamp(&q["period_end"]).unwrap_or(0.0) - now())
    ));
    lines.push(s(&r["projection"]["summary"]).into());
    lines.push(format!(
        "本机 ${:.4} / 预估周限 {}  {} 轮  {} 会话",
        n(&u["cost_usd"]),
        if e["available"] == true {
            money(number(&e["usd"]))
        } else {
            "未知".into()
        },
        n(&u["turns"]),
        n(&u["sessions"])
    ));
    lines.push(format!(
        "输入 {}（含缓存 {}）  输出 {} Token",
        n(&u["inputTokens"]),
        n(&u["cachedReadTokens"]),
        n(&u["outputTokens"])
    ));
    lines.push("官方美元及 Token 周上限：接口未公开".into());
    if e["available"] == true {
        lines.push(format!(
            "反推周限 ${:.4}（{}）；敏感区间 {} ～ {}",
            n(&e["usd"]),
            s(&e["confidence"]),
            money(number(&e["low_usd"])),
            number(&e["high_usd"])
                .map(|v| money(Some(v)))
                .unwrap_or_else(|| "无法约束上界".into())
        ));
    }
    lines.push(s(&e["reason"]).into());
    lines.push("模型成本（账本记录值，不是订阅实际扣款或官方单价）：".into());
    let mut models: Vec<_> = u["models"]
        .as_object()
        .into_iter()
        .flat_map(|v| v.iter())
        .collect();
    models.sort_by(|a, b| {
        n(&b.1["cost_usd"])
            .total_cmp(&n(&a.1["cost_usd"]))
            .then_with(|| a.0.cmp(b.0))
    });
    for (name, m) in models {
        lines.push(format!(
            "  {name}: ${:.4}  {} calls{}",
            n(&m["cost_usd"]),
            n(&m["calls"]),
            if m["partial"] == true {
                " [成本不完整]"
            } else {
                ""
            }
        ));
    }
    lines.push(format!(
        "采样：{}  {} 个样本  缓存 {} 秒",
        local_date(&r["sampled_at_iso"], "%Y-%m-%d %H:%M:%S"),
        n(&r["sample_count"]),
        n(&r["cache_age_seconds"])
    ));
    lines.push(format!(
        "去重 {} 轮  不可读账本 {}  无时间轮次 {}  不完整成本 {}",
        n(&u["duplicates"]),
        n(&u["unreadable_files"]),
        n(&u["undated_turns"]),
        n(&u["partial_cost_turns"])
    ));
    lines.push("仅统计本机已记录轮次；其他设备、未完成调用、Chat 和账户切换可能造成偏差。估算不是官方上限；敏感区间不是统计置信区间。".into());
    if r["stale"] == true {
        lines.push(format!("旧数据：{}", s(&r["error"])));
    }
    lines.join("\n")
}

pub fn display_usage(r: &Value) -> &Value {
    if r["live_local"].is_object() {
        &r["live_local"]
    } else {
        &r["local"]
    }
}
pub fn write_html(r: &Value, path: &Path) -> Result<()> {
    // Embed the template in the native executable; no runtime Python or asset lookup.
    let encoded = r
        .to_string()
        .replace('<', "\\u003c")
        .replace('&', "\\u0026");
    let html = include_str!("../grok-budget/report.html").replace("__REPORT_DATA__", &encoded);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, html)?;
    Ok(())
}
