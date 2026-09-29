//! Display-only estimates from Grok's local update stream. No transcript or
//! provisional totals are written to the quota/estimation history.
use crate::{display::Session, *};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::PathBuf,
};

const MAX_TAIL: u64 = 16 * 1024 * 1024;

#[derive(Default)]
struct Call {
    input: Option<f64>,
    output_quarters: u64,
}

struct SettledTurn {
    tokens: f64,
    cost_ticks: Option<f64>,
    at: Option<f64>,
}

fn settled_cost_ticks(settled: &BTreeMap<String, SettledTurn>) -> Option<f64> {
    if settled.is_empty() {
        return None;
    }
    let mut sum = 0.0;
    for turn in settled.values() {
        sum += turn.cost_ticks?;
    }
    Some(sum)
}

/// Same-session completed turns, used when the model id in the status payload
/// does not match ledger keys such as `grok-4.7-build`.
fn rate_from_settled(settled: &BTreeMap<String, SettledTurn>) -> Option<f64> {
    let tokens: f64 = settled.values().map(|turn| turn.tokens).sum();
    let ticks = settled_cost_ticks(settled)?;
    (tokens > 0.0).then_some(ticks / TICKS / tokens)
}

fn session_dir(home: &Path, payload: &Value) -> Option<PathBuf> {
    let id = s(&payload["session_id"]);
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    // Only read the exact session beneath this Grok home; never follow an
    // arbitrary transcript_path supplied by another process.
    let root = home.join("sessions").canonicalize().ok()?;
    let supplied = PathBuf::from(s(&payload["transcript_path"]));
    let candidates = std::iter::once(supplied).chain(
        data::ledgers(home)
            .into_iter()
            .map(|p| p.with_file_name("updates.jsonl")),
    );
    for path in candidates {
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        let parent = path.parent()?;
        if path.file_name().and_then(|v| v.to_str()) == Some("updates.jsonl")
            && parent.file_name().and_then(|v| v.to_str()) == Some(id)
            && parent.parent().and_then(Path::parent) == Some(root.as_path())
        {
            return Some(parent.to_path_buf());
        }
    }
    None
}

/// Effective same-model USD/token rate from the latest 20 completed, priced
/// model records. It includes the historical cache mix, so it is an estimate,
/// never a provider price. Unknown model/rate stays unknown.
fn historical_rate(home: &Path, model: &str) -> Option<f64> {
    if model.is_empty() {
        return None;
    }
    let mut samples = Vec::new();
    for path in data::ledgers(home) {
        let Ok(ledger) = read_json(&path) else {
            continue;
        };
        let Some(turns) = ledger["turns"].as_array() else {
            continue;
        };
        for turn in turns {
            let usage = &turn["modelUsage"][model];
            if usage["costIsPartial"] == true || n(&usage["costMissingCalls"]) > 0.0 {
                continue;
            }
            if let (Ok(ended), Some(cost), Some(tokens)) = (
                timestamp(&turn["endedAt"]),
                number(&usage["costUsdTicks"]),
                number(&usage["totalTokens"]),
            ) && tokens > 0.0
            {
                samples.push((ended, cost / TICKS, tokens));
            }
        }
    }
    samples.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (cost, tokens) = samples
        .iter()
        .take(20)
        .fold((0.0, 0.0), |(c, t), v| (c + v.1, t + v.2));
    (tokens > 0.0).then_some(cost / tokens)
}

pub fn apply(home: &Path, payload: &Value, session: &mut Session, report: Option<&mut Value>) {
    let requested = s(&payload["prompt_id"]).to_string();
    // A timer run can reuse the payload from before this turn, which has no
    // prompt id. The stream still names the open turn; follow that until a
    // payload id arrives. An explicit id always wins.
    if !requested.is_empty() {
        session.generating = true;
    }
    let Some(dir) = session_dir(home, payload) else {
        return;
    };
    let Ok(mut file) = File::open(dir.join("updates.jsonl")) else {
        return;
    };
    let Ok(metadata) = file.metadata() else {
        return;
    };
    let offset = metadata.len().saturating_sub(MAX_TAIL);
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return;
    }
    let mut reader = BufReader::new(file.take(metadata.len() - offset));
    let mut line = String::new();
    if offset > 0 && reader.read_line(&mut line).is_err() {
        return;
    }
    let mut prompt = requested.clone();
    let mut first_time = None;
    let mut turn_start = number(&payload["turn"]["started_at_ms"]).map(|v| v / 1000.0);
    let mut calls: BTreeMap<u64, Call> = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut settled: BTreeMap<String, SettledTurn> = BTreeMap::new();
    let mut current_done = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // Ignore an in-progress write; the next status run reads it once complete.
        if !line.ends_with('\n') {
            break;
        }
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if s(&event["params"]["sessionId"]) != s(&payload["session_id"]) {
            continue;
        }
        let params = &event["params"];
        let meta = &params["_meta"];
        let update = &params["update"];
        let time = number(&meta["agentTimestampMs"])
            .map(|v| v / 1000.0)
            .or_else(|| number(&event["timestamp"]));
        let from_meta = s(&meta["promptId"]);
        let event_prompt = if !from_meta.is_empty() {
            from_meta.to_string()
        } else {
            s(&update["prompt_id"]).to_string()
        };
        if requested.is_empty() && !event_prompt.is_empty() && event_prompt != prompt {
            prompt.clone_from(&event_prompt);
            calls.clear();
            seen.clear();
            current_done = false;
            turn_start = number(&payload["turn"]["started_at_ms"]).map(|v| v / 1000.0);
            first_time = None;
        }
        if first_time.is_none() {
            first_time = time;
        }
        if update["sessionUpdate"] == "turn_completed" {
            if !event_prompt.is_empty() {
                if let Some(tokens) = number(&update["usage"]["totalTokens"]) {
                    settled.insert(
                        event_prompt.clone(),
                        SettledTurn {
                            tokens,
                            cost_ticks: number(&update["usage"]["costUsdTicks"]),
                            at: time,
                        },
                    );
                }
                if event_prompt == prompt {
                    current_done = true;
                }
            }
            continue;
        }
        if event_prompt != prompt || prompt.is_empty() {
            continue;
        }
        if let Some(start) = number(&meta["turnStartMs"]) {
            turn_start = Some(start / 1000.0);
        }
        if !matches!(
            s(&update["sessionUpdate"]),
            "agent_message_chunk" | "agent_thought_chunk"
        ) || update["content"]["type"] != "text"
        {
            continue;
        }
        let event_id = s(&meta["eventId"]);
        if !event_id.is_empty() && !seen.insert(event_id.to_string()) {
            continue;
        }
        let key = number(&meta["streamStartMs"]).unwrap_or(0.0) as u64;
        let call = calls.entry(key).or_default();
        // totalTokens is context occupancy, NOT billed cumulative usage. Treat
        // it as one provisional input per model call, never add it per chunk.
        if call.input.is_none() {
            call.input = number(&meta["totalTokens"]);
        }
        for ch in s(&update["content"]["text"]).chars() {
            call.output_quarters += if ch.is_ascii() { 1 } else { 4 };
        }
    }
    if prompt.is_empty() {
        return;
    }
    // A tail that starts after this turn, or a missing start, cannot price the
    // open call. Completed usage already in the stream can still be shown.
    let mut open = !current_done;
    if open
        && (turn_start.is_none()
            || (offset > 0
                && first_time.is_none_or(|time| turn_start.is_some_and(|start| time > start))))
    {
        open = false;
        calls.clear();
    }
    if !open {
        calls.clear();
    }
    session.generating = open;
    let ledger = read_json(&dir.join("usage.json"))
        .ok()
        .filter(|ledger| s(&ledger["sessionId"]) == s(&payload["session_id"]));
    if let Some(start) = turn_start
        && ledger.as_ref().is_some_and(|ledger| {
            ledger["turns"].as_array().is_some_and(|turns| {
                turns
                    .iter()
                    .any(|t| timestamp(&t["endedAt"]).is_ok_and(|end| end >= start))
            })
        })
    {
        session.generating = false;
        return;
    }
    let settled_tokens: f64 = settled.values().map(|turn| turn.tokens).sum();
    let settled_cost = settled_cost_ticks(&settled).map(|ticks| ticks / TICKS);
    let ledger_tokens = ledger
        .as_ref()
        .and_then(|l| number(&l["session"]["totalTokens"]));
    let ledger_cost = ledger
        .as_ref()
        .and_then(|l| number(&l["session"]["costUsdTicks"]).map(|v| v / TICKS));
    // Ledger totals already include the same completed turns. Take the larger
    // of the two so a stream that has a newer settlement is not dropped, and a
    // short tail cannot replace a fuller ledger.
    let (base_tokens, base_cost) = if let Some(tokens) = ledger_tokens {
        if settled.is_empty() || tokens >= settled_tokens {
            (Some(tokens), ledger_cost)
        } else {
            (Some(settled_tokens), settled_cost.or(ledger_cost))
        }
    } else if !settled.is_empty() {
        (Some(settled_tokens), settled_cost)
    } else if !dir.join("usage.json").exists() && offset == 0 {
        (Some(0.0), Some(0.0))
    } else {
        (None, None)
    };
    let delta: f64 = calls
        .values()
        .map(|c| c.input.unwrap_or(0.0) + (c.output_quarters as f64 / 4.0).ceil())
        .sum();
    let Some(completed) = base_tokens else {
        return;
    };
    if delta == 0.0 && settled.is_empty() && ledger_tokens.is_none() {
        return;
    }
    // A newer payload may already include this call. Keep its token count
    // instead of stacking a character estimate on top. Cost is often still
    // absent at that point; price the payload from settled usage.
    let payload_tokens = session.tokens;
    let payload_ahead = payload_tokens.is_some_and(|tokens| {
        if delta > 0.0 {
            tokens > completed
        } else {
            tokens >= completed && tokens > 0.0
        }
    });
    if payload_ahead && session.cost.is_some() {
        return;
    }
    let rate =
        historical_rate(home, s(&payload["model"]["id"])).or_else(|| rate_from_settled(&settled));
    let (open_tokens, open_cost) = if payload_ahead {
        let gap = (payload_tokens.unwrap_or(0.0) - completed).max(0.0);
        let priced = match (base_cost, rate) {
            (Some(base), Some(rate)) => Some(base + rate * gap),
            (Some(base), None) if gap == 0.0 => Some(base),
            (None, Some(rate)) => payload_tokens.map(|tokens| rate * tokens),
            _ => None,
        };
        let Some(priced) = priced else {
            return;
        };
        session.cost = Some(priced);
        session.cost_estimated = gap > 0.0 || base_cost.is_none();
        (gap, rate.filter(|_| gap > 0.0).map(|rate| rate * gap))
    } else {
        let open_cost = if delta > 0.0 {
            rate.map(|value| value * delta)
        } else {
            None
        };
        session.tokens = Some(completed + delta);
        session.tokens_estimated = delta > 0.0 && session.tokens.is_some();
        if delta > 0.0 {
            // No price for the open call means the total would omit that spend.
            session.cost = base_cost.zip(open_cost).map(|(base, extra)| base + extra);
            session.cost_estimated = session.cost.is_some();
        } else if let Some(cost) = base_cost
            && (session.cost.is_none() || session.cost.is_some_and(|current| current + 1e-9 < cost))
        {
            session.cost = Some(cost);
            session.cost_estimated = false;
        }
        (delta, open_cost)
    };
    let Some(report) = report else {
        return;
    };
    let period_start = timestamp(&report["quota"]["period_start"]).unwrap_or(f64::INFINITY);
    let period_end = timestamp(&report["quota"]["period_end"]).unwrap_or(0.0);
    if now() >= period_end {
        return;
    }
    let mut extra_tokens = 0.0;
    let mut extra_cost = Some(0.0);
    // Completed turns already in a ledger are inside local_usage. Turns that
    // only exist in the stream (no usage.json yet) still belong in the total.
    if ledger.is_none() {
        for turn in settled.values() {
            let in_period = turn
                .at
                .is_none_or(|at| at >= period_start && at < period_end);
            if !in_period {
                continue;
            }
            extra_tokens += turn.tokens;
            match (extra_cost.as_mut(), turn.cost_ticks) {
                (Some(slot), Some(ticks)) => *slot += ticks / TICKS,
                _ => extra_cost = None,
            }
        }
    }
    if open_tokens > 0.0
        && turn_start.is_some_and(|start| start >= period_start && start < period_end)
    {
        extra_tokens += open_tokens;
        match (extra_cost.as_mut(), open_cost) {
            (Some(slot), Some(value)) => *slot += value,
            _ => extra_cost = None,
        }
    }
    if extra_tokens <= 0.0 && extra_cost.unwrap_or(0.0) <= 0.0 {
        return;
    }
    // Re-read completed local totals after the stream scan, so the overlay
    // never stacks on a stale local-cache baseline. Recheck settlement to
    // avoid double counting a turn that ended during this status run.
    let usage = data::local_usage(home, period_start, now());
    if turn_start.is_some_and(|start| {
        read_json(&dir.join("usage.json"))
            .ok()
            .is_some_and(|ledger| {
                ledger["turns"].as_array().is_some_and(|turns| {
                    turns
                        .iter()
                        .any(|t| timestamp(&t["endedAt"]).is_ok_and(|end| end >= start))
                })
            })
    }) {
        *session = crate::display::session_usage(home, payload);
        report["live_local"] = usage;
        return;
    }
    report["live_local"] = usage;
    let usage = &mut report["live_local"];
    usage["totalTokens"] = json!(n(&usage["totalTokens"]) + extra_tokens);
    usage["tokens_estimated"] = json!(open_tokens > 0.0 && !payload_ahead);
    match extra_cost {
        Some(value) => {
            usage["cost_usd"] = json!(n(&usage["cost_usd"]) + value);
            usage["cost_estimated"] = json!(open_cost.is_some() && open_tokens > 0.0);
        }
        None => {
            usage["cost_usd"] = Value::Null;
            usage["cost_estimated"] = json!(false);
        }
    }
}
