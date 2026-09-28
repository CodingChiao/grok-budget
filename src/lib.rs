pub mod data;
pub mod display;
pub mod storage;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, Utc};
use serde_json::Value;
use std::{fs, path::Path};

pub const ENDPOINT: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
pub const TICKS: f64 = 10_000_000_000.0;
pub const FIELDS: [&str; 7] = [
    "inputTokens",
    "outputTokens",
    "cachedReadTokens",
    "cacheCreationTokens",
    "reasoningTokens",
    "totalTokens",
    "modelCalls",
];

pub fn number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite() && *n >= 0.0)
}
pub fn n(v: &Value) -> f64 {
    number(v).unwrap_or(0.0)
}
pub fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
pub fn now() -> f64 {
    Utc::now().timestamp_millis() as f64 / 1000.0
}
pub fn timestamp(v: &Value) -> Result<f64> {
    let d = DateTime::parse_from_rfc3339(s(v)).context("日期格式无效或缺少时区")?;
    Ok(d.timestamp() as f64 + d.timestamp_subsec_nanos() as f64 / 1e9)
}
pub fn iso(t: f64) -> String {
    DateTime::from_timestamp_millis((t * 1000.0) as i64)
        .unwrap_or_default()
        .to_rfc3339()
}
pub fn local_date(v: &Value, format: &str) -> String {
    DateTime::parse_from_rfc3339(s(v))
        .map(|d| d.with_timezone(&Local).format(format).to_string())
        .unwrap_or_else(|_| "未知".into())
}
pub fn read_json(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(raw.trim_start_matches('\u{feff}'))?)
}
pub fn credentials(home: &Path) -> Result<(String, String)> {
    use sha2::{Digest, Sha256};
    let auth = read_json(&home.join("auth.json"))
        .map_err(|_| anyhow::anyhow!("无法读取 Grok 登录状态，请先运行 grok login。"))?;
    let entries: Vec<_> = auth
        .as_object()
        .context("登录格式无效")?
        .values()
        .filter(|v| !s(&v["key"]).is_empty() && matches!(s(&v["auth_mode"]), "oidc" | "session"))
        .collect();
    if entries.len() != 1 {
        bail!("未找到唯一的 Grok 登录项，请运行 grok login 确认账户。");
    }
    let entry = entries[0];
    let identity = if !entry["user_id"].is_null() {
        &entry["user_id"]
    } else {
        &entry["principal_id"]
    };
    if identity.is_null() || s(identity).is_empty() {
        bail!("登录项缺少账户标识。");
    }
    Ok((
        s(&entry["key"]).into(),
        format!("{:x}", Sha256::digest(s(identity).as_bytes()))[..24].into(),
    ))
}
