use grok_budget::{display::*, live, *};
use serde_json::{Value, json};
use std::{fs, io::Write, path::PathBuf};
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
    stream: PathBuf,
    ledger: PathBuf,
    payload: Value,
    report: Value,
    start: f64,
}
impl Fixture {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("sessions/workspace/active");
        fs::create_dir_all(&dir).unwrap();
        let start = now() - 10.0;
        let ledger = dir.join("usage.json");
        let usage = json!({"inputTokens":900,"outputTokens":100,"totalTokens":1000,"costUsdTicks":1_000_000_000u64});
        fs::write(&ledger, json!({"sessionId":"active", "session":usage,
            "turns":[{"endedAt":iso(start-3600.0),"turnNumber":1,"inputTokens":900,"outputTokens":100,
                "totalTokens":1000,"costUsdTicks":1_000_000_000u64,"primaryModelId":"model",
                "modelUsage":{"model":usage}}]}).to_string()).unwrap();
        let stream = dir.join("updates.jsonl");
        fs::write(&stream, "").unwrap();
        let payload = json!({"session_id":"active","prompt_id":"prompt","model":{"id":"model"},
            "turn":{"started_at_ms":start*1000.0},"transcript_path":stream,
            "cost":{"total_cost_usd":0.1},"context_window":{"session_input_tokens":900,"session_output_tokens":100}});
        let report = json!({"sampled_at":start,"quota":{"period_start":iso(start-86400.0),"period_end":iso(start+86400.0),"used_percent":10},
            "local":{"cost_usd":0.1,"totalTokens":1000},"estimate":{"available":true,"usd":10}});
        Self {
            home,
            stream,
            ledger,
            payload,
            report,
            start,
        }
    }
    fn chunk(&self, id: &str, call: u64, text: &str) -> Value {
        json!({"timestamp":self.start+1.0,"params":{"sessionId":"active",
            "_meta":{"eventId":id,"promptId":"prompt","turnStartMs":self.start*1000.0,
                "streamStartMs":call,"totalTokens":100},
            "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}})
    }
    fn append(&self, event: &Value) {
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(&self.stream)
                .unwrap(),
            "{event}"
        )
        .unwrap();
    }
    fn render(&self) -> (Session, Value) {
        let mut session = session_usage(self.home.path(), &self.payload);
        let mut report = self.report.clone();
        live::apply(
            self.home.path(),
            &self.payload,
            &mut session,
            Some(&mut report),
        );
        (session, report)
    }
}

#[test]
fn grows_with_stream_and_reconciles_without_polluting_history() {
    let f = Fixture::new();
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1102.0));
    assert!((s.cost.unwrap() - 0.1102).abs() < 1e-8);
    assert!(s.tokens_estimated && s.cost_estimated && s.generating);
    assert_eq!(r["live_local"]["totalTokens"], 1102.0);
    assert_eq!(r["local"], f.report["local"]);
    assert_eq!(r["estimate"], f.report["estimate"]);
    assert_eq!(r["quota"], f.report["quota"]);
    let colored = status_text(Some(&r), &s, 160, true);
    assert!(colored.contains("\x1b[36m≈$0.1102"));
    assert!(colored.contains("\x1b[35m≈1102"));
    assert!(colored.contains("生成中"));

    f.append(&f.chunk("b", 1, "中文ab"));
    f.append(&f.chunk("b", 1, "中文ab")); // duplicate transport event
    let mut other = f.chunk("foreign", 1, "ignore");
    other["params"]["sessionId"] = json!("other");
    f.append(&other);
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1105.0)); // ceil((8 + 8 + 2)/4), one input
    assert_eq!(r["live_local"]["totalTokens"], 1105.0);
    f.append(&f.chunk("c", 2, "abcd"));
    assert_eq!(f.render().0.tokens, Some(1206.0)); // next call adds input once

    // Ledger settlement wins even if the status payload still says generating.
    let mut ledger = read_json(&f.ledger).unwrap();
    ledger["session"]["totalTokens"] = json!(1250);
    ledger["session"]["costUsdTicks"] = json!(1_300_000_000u64);
    ledger["turns"]
        .as_array_mut()
        .unwrap()
        .push(json!({"turnNumber":2,"endedAt":iso(f.start+2.0),
        "totalTokens":250,"costUsdTicks":300_000_000u64}));
    fs::write(&f.ledger, ledger.to_string()).unwrap();
    let (s, _) = f.render();
    assert_eq!(s.tokens, Some(1250.0));
    assert_eq!(s.cost, Some(0.13));
    assert!(!s.tokens_estimated && !s.cost_estimated && !s.generating);
}

#[test]
fn completion_missing_history_and_partial_lines_are_safe() {
    let mut f = Fixture::new();
    f.payload["model"]["id"] = json!("unknown-model");
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1102.0));
    assert!(s.cost.is_none());
    assert!(r["live_local"]["cost_usd"].is_null());
    let fragment = f.chunk("partial", 1, "abcdefgh").to_string();
    write!(
        fs::OpenOptions::new().append(true).open(&f.stream).unwrap(),
        "{fragment}"
    )
    .unwrap();
    assert_eq!(f.render().0.tokens, Some(1102.0));
    writeln!(fs::OpenOptions::new().append(true).open(&f.stream).unwrap()).unwrap();
    assert_eq!(f.render().0.tokens, Some(1104.0));
    f.append(&json!({"params":{"sessionId":"active","update":{"sessionUpdate":"turn_completed","prompt_id":"prompt"}}}));
    let (s, r) = f.render();
    assert!(!s.generating && !s.tokens_estimated);
    assert!(r["live_local"].is_null());
}

#[test]
fn idle_newer_payload_and_previous_prompt_are_not_double_counted() {
    let mut f = Fixture::new();
    let mut old = f.chunk("old", 1, "old text");
    old["params"]["_meta"]["promptId"] = json!("previous");
    f.append(&old);
    assert!(!f.render().0.tokens_estimated);
    f.append(&f.chunk("a", 1, "abcdefgh"));
    f.payload["context_window"]["session_output_tokens"] = json!(200);
    let (s, _) = f.render();
    assert_eq!(s.tokens, Some(1100.0));
    assert!(!s.tokens_estimated);
    f.append(&json!({"params":{"sessionId":"active","update":{"sessionUpdate":"turn_completed","prompt_id":"prompt"}}}));
    f.payload.as_object_mut().unwrap().remove("prompt_id");
    let idle = f.render().0;
    assert!(!idle.generating && !idle.tokens_estimated);
}

#[test]
fn payload_tokens_ahead_of_the_stream_keep_a_priced_session_cost() {
    let mut f = Fixture::new();
    fs::remove_file(&f.ledger).unwrap();
    f.payload.as_object_mut().unwrap().remove("cost");
    f.payload["context_window"]["session_input_tokens"] = json!(1500);
    f.payload["context_window"]["session_output_tokens"] = json!(0);
    f.append(&json!({"timestamp":f.start,"params":{"sessionId":"active",
        "_meta":{"agentTimestampMs":f.start*1000.0},
        "update":{"sessionUpdate":"turn_completed","prompt_id":"previous",
            "usage":{"totalTokens":1000,"costUsdTicks":1_000_000_000u64}}}}));
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1500.0));
    assert!(!s.tokens_estimated);
    assert!((s.cost.unwrap() - 0.15).abs() < 1e-8);
    assert!(s.cost_estimated && s.generating);
    assert_eq!(r["live_local"]["totalTokens"], 1500.0);
    assert!((r["live_local"]["cost_usd"].as_f64().unwrap() - 0.15).abs() < 1e-8);
    assert_eq!(r["live_local"]["tokens_estimated"], false);
    assert_eq!(r["live_local"]["cost_estimated"], true);

    f.payload["cost"]["total_cost_usd"] = json!(0.2);
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1500.0));
    assert_eq!(s.cost, Some(0.2));
    assert!(!s.cost_estimated);
    assert!(r["live_local"].is_null());
}

#[test]
fn new_conversation_prices_client_tokens_from_the_billing_model_suffix() {
    let mut f = Fixture::new();
    fs::remove_file(&f.ledger).unwrap();
    let older = f.home.path().join("sessions/workspace/older");
    fs::create_dir_all(&older).unwrap();
    let usage = json!({"totalTokens":1000,"costUsdTicks":1_000_000_000u64});
    fs::write(
        older.join("usage.json"),
        json!({"sessionId":"older","turns":[{"endedAt":iso(f.start-3600.0),
            "totalTokens":1000,"costUsdTicks":1_000_000_000u64,"primaryModelId":"grok-4.7-build",
            "modelUsage":{"grok-4.7-build":usage}}]})
        .to_string(),
    )
    .unwrap();
    f.payload["model"]["id"] = json!("grok-4.7");
    f.payload.as_object_mut().unwrap().remove("cost");
    f.payload["context_window"]["session_input_tokens"] = json!(1500);
    f.payload["context_window"]["session_output_tokens"] = json!(0);
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1500.0));
    assert!(!s.tokens_estimated);
    assert!(
        (s.cost.unwrap() - 0.15).abs() < 1e-8,
        "session cost {}",
        s.cost.unwrap()
    );
    assert!(s.cost_estimated && s.generating);
    assert!((r["live_local"]["cost_usd"].as_f64().unwrap() - 0.25).abs() < 1e-8);
    assert_eq!(r["live_local"]["totalTokens"], 2500.0);
    assert_eq!(r["live_local"]["cost_estimated"], true);

    // A shorter id must not inherit a different model's price.
    f.payload["model"]["id"] = json!("grok-4");
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1500.0));
    assert!(s.cost.is_none());
    assert!(r["live_local"].is_null());
}

#[test]
fn ahead_payload_without_a_rate_leaves_cost_unknown() {
    let mut f = Fixture::new();
    fs::remove_file(&f.ledger).unwrap();
    f.payload["model"]["id"] = json!("unknown-model");
    f.payload.as_object_mut().unwrap().remove("cost");
    f.payload["context_window"]["session_input_tokens"] = json!(1500);
    f.payload["context_window"]["session_output_tokens"] = json!(0);
    f.append(&json!({"timestamp":f.start,"params":{"sessionId":"active",
        "update":{"sessionUpdate":"turn_completed","prompt_id":"previous",
            "usage":{"totalTokens":1000}}}}));
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1500.0));
    assert!(s.cost.is_none());
    assert!(r["live_local"].is_null());
}

#[test]
fn settled_stream_usage_is_the_base_when_the_ledger_file_is_missing() {
    let mut f = Fixture::new();
    fs::remove_file(&f.ledger).unwrap();
    f.payload["context_window"]["session_input_tokens"] = json!(0);
    f.payload["context_window"]["session_output_tokens"] = json!(0);
    f.payload["cost"]["total_cost_usd"] = json!(0);
    f.append(&json!({"timestamp":f.start,"params":{"sessionId":"active",
        "_meta":{"agentTimestampMs":f.start*1000.0},
        "update":{"sessionUpdate":"turn_completed","prompt_id":"previous",
            "usage":{"totalTokens":1000,"costUsdTicks":1_000_000_000u64}}}}));
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let (s, r) = f.render();
    assert_eq!(
        s.tokens,
        Some(1102.0),
        "completed usage must not be discarded"
    );
    assert!((s.cost.unwrap() - 0.1102).abs() < 1e-8);
    assert!(s.tokens_estimated && s.cost_estimated && s.generating);
    assert_eq!(r["live_local"]["totalTokens"], 1102.0);

    f.append(&f.chunk("b", 1, "abcdefgh"));
    assert_eq!(f.render().0.tokens, Some(1104.0));
    // A refresh can reuse the pre-turn payload, which has no prompt id.
    f.payload.as_object_mut().unwrap().remove("prompt_id");
    let (s, _) = f.render();
    assert_eq!(s.tokens, Some(1104.0));
    assert!(s.generating && s.tokens_estimated);

    f.payload["prompt_id"] = json!("prompt");
    f.append(
        &json!({"timestamp":f.start+2.0,"params":{"sessionId":"active",
        "update":{"sessionUpdate":"turn_completed","prompt_id":"prompt",
            "usage":{"totalTokens":250,"costUsdTicks":300_000_000u64}}}}),
    );
    let (s, _) = f.render();
    assert_eq!(s.tokens, Some(1250.0));
    assert!((s.cost.unwrap() - 0.13).abs() < 1e-8);
    assert!(!s.tokens_estimated && !s.cost_estimated && !s.generating);
}

#[test]
fn stream_settlement_does_not_double_count_the_ledger() {
    let f = Fixture::new();
    f.append(&json!({"timestamp":f.start,"params":{"sessionId":"active",
        "update":{"sessionUpdate":"turn_completed","prompt_id":"previous",
            "usage":{"totalTokens":1000,"costUsdTicks":1_000_000_000u64}}}}));
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1102.0));
    assert_eq!(r["live_local"]["totalTokens"], 1102.0);
}

#[test]
fn new_session_can_estimate_tokens_but_cross_period_overlay_is_rejected() {
    let mut f = Fixture::new();
    fs::remove_file(&f.ledger).unwrap();
    f.payload.as_object_mut().unwrap().remove("context_window");
    f.payload.as_object_mut().unwrap().remove("cost");
    f.append(&f.chunk("a", 1, "abcd"));
    let (s, _) = f.render();
    assert_eq!(s.tokens, Some(101.0));
    assert!(s.cost.is_none());
    f.report["quota"]["period_start"] = json!(iso(f.start + 1.0));
    assert!(f.render().1["live_local"].is_null());
}

#[test]
fn open_turn_shows_without_transcript_path_or_ledger() {
    let mut f = Fixture::new();
    fs::remove_file(&f.ledger).unwrap();
    f.payload.as_object_mut().unwrap().remove("transcript_path");
    f.payload.as_object_mut().unwrap().remove("prompt_id");
    f.payload.as_object_mut().unwrap().remove("cost");
    f.payload["trigger"] = json!("refresh_interval");
    f.payload["context_window"]["session_input_tokens"] = json!(0);
    f.payload["context_window"]["session_output_tokens"] = json!(0);
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let (s, _) = f.render();
    assert_eq!(s.tokens, Some(102.0));
    assert!(s.generating && s.tokens_estimated);
}

#[test]
fn refresh_snapshot_does_not_hide_a_larger_open_turn() {
    let mut f = Fixture::new();
    fs::remove_file(&f.ledger).unwrap();
    f.payload.as_object_mut().unwrap().remove("prompt_id");
    f.payload["trigger"] = json!("refresh_interval");
    f.payload["cost"]["total_cost_usd"] = json!(0.12);
    f.payload["context_window"]["session_input_tokens"] = json!(1050);
    f.payload["context_window"]["session_output_tokens"] = json!(0);
    f.append(&json!({"timestamp":f.start,"params":{"sessionId":"active",
        "_meta":{"agentTimestampMs":f.start*1000.0},
        "update":{"sessionUpdate":"turn_completed","prompt_id":"previous",
            "usage":{"totalTokens":1000,"costUsdTicks":1_000_000_000u64}}}}));
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let (s, _) = f.render();
    assert_eq!(s.tokens, Some(1102.0));
    assert!(s.generating && s.tokens_estimated);
}

#[test]
fn external_transcript_cannot_be_used_as_session_data() {
    let mut f = Fixture::new();
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let external = TempDir::new().unwrap();
    fs::create_dir(external.path().join("sessions")).unwrap();
    f.payload["session_id"] = json!("../active");
    let mut s = Session::default();
    live::apply(external.path(), &f.payload, &mut s, None);
    assert!(!s.tokens_estimated);
}

#[test]
fn installed_command_pipeline_updates_both_rows_with_fixed_stdin() {
    let f = Fixture::new();
    fs::write(
        f.home.path().join("auth.json"),
        json!({"issuer":{"auth_mode":"oidc","key":"fixture-only","user_id":"fixture"}}).to_string(),
    )
    .unwrap();
    grok_budget::storage::collect_at(
        f.home.path(),
        &f.home.path().join("grok-budget"),
        grok_budget::storage::Options::default(),
        now(),
        |_| Ok(f.report["quota"].clone()),
    )
    .unwrap();
    let run = || {
        use std::process::{Command, Stdio};
        let mut child = Command::new(env!("CARGO_BIN_EXE_grok-budget"))
            .args(["--statusline", "--no-color", "--grok-home"])
            .arg(f.home.path())
            .env("COLUMNS", "160")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(f.payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let first = run();
    assert!(first.contains("会话 ≈$0.1102 · ≈1102 Token"), "{first}");
    assert!(first.contains("已用 ≈$0.1102 · ≈1102 Token"), "{first}");
    f.append(&f.chunk("b", 1, "abcdefgh"));
    let second = run();
    assert!(second.contains("会话 ≈$0.1104 · ≈1104 Token"), "{second}");
    assert!(second.contains("已用 ≈$0.1104 · ≈1104 Token"), "{second}");
    // Neither a changing stdout estimate nor repeated runs create quota samples.
    let db = grok_budget::storage::connect(&f.home.path().join("grok-budget")).unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM snapshots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn unreadable_existing_ledger_is_not_treated_as_a_new_session() {
    let f = Fixture::new();
    fs::write(&f.ledger, "{").unwrap();
    f.append(&f.chunk("a", 1, "abcdefgh"));
    let (s, r) = f.render();
    assert_eq!(s.tokens, Some(1000.0));
    assert!(!s.tokens_estimated);
    assert!(r["live_local"].is_null());
}
