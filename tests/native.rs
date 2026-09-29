use grok_budget::{data::*, display::*, storage::*, *};
use serde_json::{Value, json};
use std::{fs, path::Path};
use tempfile::TempDir;

const START: &str = "2026-09-28T16:08:48+00:00";
const END: &str = "2026-10-05T16:08:48+00:00";
fn at() -> f64 {
    timestamp(&json!("2026-09-29T02:00:00+00:00")).unwrap()
}
fn billing(p: f64) -> Value {
    json!({"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":START,"end":END},"creditUsagePercent":p,"isUnifiedBillingUser":true,"productUsage":[{"product":"GrokBuild","usagePercent":p},{"product":"GrokChat"}],"unrelated_secret":"do not retain"}})
}
fn turn() -> Value {
    json!({"turnNumber":1,"endedAt":"2026-09-28T17:00:00Z","costUsdTicks":1_000_000_000u64,"inputTokens":100,"cachedReadTokens":80,"outputTokens":10,"totalTokens":110,"modelCalls":1,"primaryModelId":"grok-test"})
}
fn snapshot(p: f64, c: f64, t: f64) -> Value {
    json!({"account":"account","sampled_at":t,"quota":normalize_billing(&billing(p)).unwrap(),"local":{"cost_usd":c,"totalTokens":12000,"unreadable_files":0,"undated_turns":0,"partial_cost_turns":0}})
}
fn ledger(home: &Path, id: &str, turns: Vec<Value>) -> std::path::PathBuf {
    let path = home
        .join("sessions")
        .join("workspace")
        .join(id)
        .join("usage.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path,json!({"sessionId":id,"turns":turns,"session":{"costUsdTicks":2_500_000_000u64,"totalTokens":1250}}).to_string()).unwrap();
    path
}
fn auth(home: &Path, user: &str) {
    fs::write(
        home.join("auth.json"),
        json!({"issuer":{"key":"SECRET_TOKEN","auth_mode":"oidc","user_id":user}}).to_string(),
    )
    .unwrap();
}
#[test]
fn percentage_units_and_allowlist() {
    let q = normalize_billing(&billing(1.0)).unwrap();
    assert_eq!(q["used_percent"], 1.0);
    assert_eq!(q["remaining_percent"], 99.0);
    assert!(q["official_limit_usd"].is_null());
    assert!(!q.to_string().contains("do not retain"));
    for value in [Value::Null, json!(-1), json!(101), json!(true), json!("1")] {
        let mut b = billing(1.0);
        b["config"]["creditUsagePercent"] = value;
        assert!(normalize_billing(&b).is_err());
    }
    let mut b = billing(1.0);
    b["config"]["currentPeriod"]["start"] = json!("2026-09-28T16:00:00");
    assert!(normalize_billing(&b).is_err());
}
#[test]
fn dedup_boundary_and_cache_tokens() {
    let tmp = TempDir::new().unwrap();
    let mut later = turn();
    later["costUsdTicks"] = json!(2_000_000_000u64);
    later["endedAt"] = json!("2026-09-28T18:00:00Z");
    let mut boundary = turn();
    boundary["endedAt"] = json!(END);
    ledger(tmp.path(), "original", vec![turn(), boundary]);
    ledger(tmp.path(), "fork", vec![turn(), later]);
    let u = local_usage(
        tmp.path(),
        timestamp(&json!(START)).unwrap(),
        timestamp(&json!(END)).unwrap(),
    );
    assert!((n(&u["cost_usd"]) - 0.3).abs() < 1e-12);
    assert_eq!(u["duplicates"], 1.0);
    assert_eq!(u["turns"], 2.0);
    assert_eq!(u["inputTokens"], 200.0);
    assert_eq!(u["totalTokens"], 220.0);
}
#[test]
fn bad_and_partial_ledgers_disable_estimation() {
    let tmp = TempDir::new().unwrap();
    let p = ledger(tmp.path(), "bad", vec![]);
    fs::write(p, "{").unwrap();
    let mut missing = turn();
    missing["costUsdTicks"] = Value::Null;
    let mut undated = turn();
    undated["endedAt"] = Value::Null;
    ledger(tmp.path(), "partial", vec![missing, undated]);
    let u = local_usage(tmp.path(), timestamp(&json!(START)).unwrap(), at());
    assert_eq!(u["unreadable_files"], 1.0);
    assert_eq!(u["undated_turns"], 1.0);
    assert_eq!(u["partial_cost_turns"], 1.0);
    let mut r = snapshot(10.0, 5.0, at());
    r["local"] = u;
    assert_eq!(estimate(&r, &[])["available"], false);
}
#[test]
fn period_delta_and_unbounded_sensitivity() {
    let e = estimate(&snapshot(1.0, 0.5, at()), &[]);
    assert_eq!(e["usd"], 50.0);
    assert!(e["high_usd"].is_null());
    assert_eq!(e["method"], "period");
    let e = estimate(
        &snapshot(10.0, 5.0, at()),
        &[snapshot(2.0, 1.0, at() - 1000.0)],
    );
    assert_eq!(e["method"], "delta");
    assert_eq!(e["usd"], 50.0);
    assert_eq!(e["low_usd"], 40.0);
}
#[test]
fn chat_saturation_and_period_guards() {
    for p in [0.0, 100.0] {
        assert_eq!(estimate(&snapshot(p, 0.5, at()), &[])["available"], false);
    }
    let mut r = snapshot(2.0, 0.5, at());
    r["quota"]["products"] = json!([{"product":"GrokChat","used_percent":1}]);
    assert_eq!(estimate(&r, &[])["available"], false);
    r["quota"]["products"] =
        json!([{"product":"GrokBuild","used_percent":1},{"product":"GrokChat","used_percent":1}]);
    assert_eq!(estimate(&r, &[])["usd"], 50.0);
    r["quota"]["period_type"] = json!("DAILY");
    assert_eq!(estimate(&r, &[])["available"], false);
}
#[test]
fn account_reset_and_rollback_isolation() {
    let r = snapshot(10.0, 5.0, at());
    let mut old = snapshot(2.0, 1.0, at() - 1000.0);
    old["account"] = json!("other");
    assert_eq!(estimate(&r, &[old])["method"], "period");
    let mut old = snapshot(2.0, 1.0, at() - 1000.0);
    old["quota"]["period_start"] = json!("2026-09-20T00:00:00Z");
    assert_eq!(estimate(&r, &[old])["method"], "period");
    let h = vec![
        snapshot(2.0, 1.0, at() - 7200.0),
        snapshot(15.0, 7.5, at() - 3600.0),
    ];
    assert_eq!(estimate(&r, &h)["method"], "period");
    assert_eq!(extrapolate(&r, &h)["available"], false);
}
#[test]
fn pace_requires_time_and_change() {
    let r = snapshot(10.0, 5.0, at());
    assert_eq!(
        extrapolate(&r, &[snapshot(2.0, 1.0, at() - 1000.0)])["available"],
        false
    );
    assert_eq!(
        extrapolate(&r, &[snapshot(9.4, 4.0, at() - 7200.0)])["available"],
        false
    );
    let p = extrapolate(&r, &[snapshot(2.0, 1.0, at() - 7200.0)]);
    assert_eq!(p["percent_per_hour"], 4.0);
    assert_eq!(p["hours_to_full"], 22.5);
}
#[test]
fn storage_reuses_cache_and_does_not_hold_network_lock() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a-user");
    ledger(tmp.path(), "a", vec![turn()]);
    let dir = tmp.path().join("data");
    let r = collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        let other = connect(&dir).unwrap();
        other.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    let r2 = collect_at(tmp.path(), &dir, Options::default(), at() + 10.0, |_| {
        panic!("cache hit must not fetch")
    })
    .unwrap();
    assert_eq!(r["sampled_at"], r2["sampled_at"]);
    // With WAL, recent rows may still sit in the -wal file until a checkpoint.
    let mut raw = fs::read(dir.join("history.sqlite3")).unwrap();
    if let Ok(wal) = fs::read(dir.join("history.sqlite3-wal")) {
        raw.extend(wal);
    }
    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.contains("snapshots"),
        "database content must be scanned"
    );
    assert!(!text.contains("SECRET_TOKEN"));
    assert!(!text.contains("a-user"));
    auth(tmp.path(), "other-user");
    assert!(
        collect_at(
            tmp.path(),
            &dir,
            Options {
                offline: true,
                ..Default::default()
            },
            at(),
            |_| panic!()
        )
        .is_err()
    );
}
#[test]
fn failure_and_expiry_preserve_visible_stale_cache() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    let r = collect_at(tmp.path(), &dir, Options::default(), at() + 120.0, |_| {
        anyhow::bail!("offline")
    })
    .unwrap();
    assert_eq!(r["stale"], true);
    assert_eq!(r["error"], "offline");
    let r = collect_at(
        tmp.path(),
        &dir,
        Options {
            offline: true,
            ..Default::default()
        },
        at() + 121.0,
        |_| panic!(),
    )
    .unwrap();
    assert_eq!(r["stale"], true);
    let r = collect_at(
        tmp.path(),
        &dir,
        Options::default(),
        timestamp(&json!(END)).unwrap() + 1.0,
        |_| normalize_billing(&billing(1.0)),
    )
    .unwrap();
    assert_eq!(r["stale"], true);
    assert_eq!(r["sampled_at"], at());
}
#[test]
fn compatible_python_database_schema() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    let db = connect(&dir).unwrap();
    let (_, account) = credentials(tmp.path()).unwrap();
    let mut r = snapshot(1.0, 0.5, at());
    r["account"] = json!(account);
    db.execute(
        "INSERT INTO snapshots(account,sampled,payload) VALUES (?,?,?)",
        rusqlite::params![account, at(), r.to_string()],
    )
    .unwrap();
    let cached = collect_at(
        tmp.path(),
        &dir,
        Options {
            offline: true,
            ..Default::default()
        },
        at(),
        |_| panic!(),
    )
    .unwrap();
    assert_eq!(cached["local"]["cost_usd"], 0.5);
}
#[test]
fn session_cost_fallback_matches_exact_session() {
    let tmp = TempDir::new().unwrap();
    ledger(tmp.path(), "active", vec![turn()]);
    ledger(tmp.path(), "other", vec![turn()]);
    let session = session_usage(
        tmp.path(),
        &json!({"session_id":"active","context_window":{"session_input_tokens":1100,"session_output_tokens":150}}),
    );
    assert_eq!(session.cost, Some(0.25));
    assert_eq!(session.tokens, Some(1250.0));
    let explicit = session_usage(
        tmp.path(),
        &json!({"session_id":"active","cost":{"total_cost_usd":0},"context_window":{"session_input_tokens":1100,"session_output_tokens":150}}),
    );
    assert_eq!(explicit.cost, Some(0.25));
    let missing = session_usage(tmp.path(), &json!({"session_id":"unknown"}));
    assert!(missing.cost.is_none());
    let traversal = session_usage(tmp.path(), &json!({"session_id":"../active"}));
    assert!(traversal.cost.is_none());
}

#[test]
fn resumed_session_uses_matching_ledger_cost_and_tokens() {
    let tmp = TempDir::new().unwrap();
    ledger(tmp.path(), "resumed", vec![turn()]);
    let session = session_usage(
        tmp.path(),
        &json!({"session_id":"resumed",
        "cost":{"total_duration_ms":43,"total_api_duration_ms":0},
        "context_window":{"session_input_tokens":0,"session_output_tokens":0}}),
    );
    assert_eq!(session.cost, Some(0.25));
    assert_eq!(session.tokens, Some(1250.0));
}

#[test]
fn session_timer_reads_new_ledger_with_unchanged_complete_payload() {
    let tmp = TempDir::new().unwrap();
    let path = ledger(tmp.path(), "active", vec![turn()]);
    let payload = json!({"session_id":"active", "trigger":"refresh_interval",
        "cost":{"total_cost_usd":0.25},
        "context_window":{"session_input_tokens":1100,"session_output_tokens":150}});
    assert_eq!(session_usage(tmp.path(), &payload).tokens, Some(1250.0));
    let mut contents = read_json(&path).unwrap();
    contents["session"]["totalTokens"] = json!(2500);
    contents["session"]["costUsdTicks"] = json!(5_000_000_000u64);
    fs::write(&path, contents.to_string()).unwrap();
    let updated = session_usage(tmp.path(), &payload);
    assert_eq!(updated.tokens, Some(2500.0));
    assert_eq!(updated.cost, Some(0.5));

    // A fresh client payload can precede the ledger write; do not roll it back.
    let newer = json!({"session_id":"active", "cost":{"total_cost_usd":0.75},
        "context_window":{"session_input_tokens":3000,"session_output_tokens":750}});
    assert_eq!(session_usage(tmp.path(), &newer).tokens, Some(3750.0));
    assert_eq!(session_usage(tmp.path(), &newer).cost, Some(0.75));
    let mut unpriced = newer;
    unpriced.as_object_mut().unwrap().remove("cost");
    assert_eq!(session_usage(tmp.path(), &unpriced).tokens, Some(3750.0));
    assert!(session_usage(tmp.path(), &unpriced).cost.is_none());

    // New unpriced usage must not retain an old amount as if it priced the new total.
    contents["session"]
        .as_object_mut()
        .unwrap()
        .remove("costUsdTicks");
    fs::write(&path, contents.to_string()).unwrap();
    assert!(session_usage(tmp.path(), &payload).cost.is_none());
}

#[test]
fn cumulative_tokens_refresh_without_waiting_for_quota_or_mutating_samples() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let path = ledger(tmp.path(), "active", vec![turn()]);
    let dir = tmp.path().join("data");
    let mut report = collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    let estimate_before = report["estimate"].clone();
    let mut next = turn();
    next["endedAt"] = json!(iso(at() + 1.0));
    let mut contents = read_json(&path).unwrap();
    contents["turns"].as_array_mut().unwrap().push(next);
    fs::write(path, contents.to_string()).unwrap();
    refresh_local_cached(tmp.path(), &dir, &mut report, at() + 2.0).unwrap();
    assert_eq!(display_usage(&report)["totalTokens"], 220.0);
    assert_eq!(display_usage(&report)["cost_usd"], 0.2);
    assert_eq!(report["local"]["totalTokens"], 110.0);
    assert_eq!(report["estimate"], estimate_before);
    let text = status_text(Some(&report), &Session::default(), 100, false);
    assert!(text.contains("已用 $0.20 · 220 Token"));
    let cached = collect_at(tmp.path(), &dir, Options::default(), at() + 3.0, |_| {
        panic!("quota must stay cached")
    })
    .unwrap();
    assert_eq!(cached["local"]["totalTokens"], 110.0);
    assert!(cached["live_local"].is_null());
}
#[test]
fn session_cache_tokens_counted_once() {
    let tmp = TempDir::new().unwrap();
    let p = json!({"context_window":{"session_input_tokens":1000,"session_output_tokens":20,"session_usage":{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":50,"cache_read_input_tokens":850}}});
    assert_eq!(session_usage(tmp.path(), &p).tokens, Some(1020.0));
    let mut p = p;
    p["context_window"]
        .as_object_mut()
        .unwrap()
        .remove("session_input_tokens");
    assert_eq!(session_usage(tmp.path(), &p).tokens, Some(1020.0));
    p["context_window"]["session_usage"]
        .as_object_mut()
        .unwrap()
        .remove("output_tokens");
    assert!(session_usage(tmp.path(), &p).tokens.is_none());
}
fn strip_ansi(s: &str) -> String {
    let mut plain = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}
fn visible_width(s: &str) -> usize {
    width(&strip_ansi(s))
}
#[test]
fn session_remains_when_account_quota_unavailable() {
    let session = Session {
        cost: Some(0.85),
        tokens: Some(3_721_843.0),
        partial: false,
        ..Session::default()
    };
    let line = status_text(None, &session, 80, true);
    let first = line.lines().next().unwrap();
    assert!(first.contains("\x1b[36m$0.85\x1b[0m"));
    assert!(first.contains("\x1b[35m3.72M\x1b[0m"));
    assert!(strip_ansi(first).contains("会话 $0.85 · 3.72M Token"));
    assert!(line.contains("暂不可用"));
}
#[test]
fn zero_value_spans_color_cleanly() {
    // 单字符 "0" 会先命中 \x1b[0m 里的 0；着色必须按位置单次扫描，不能事后替换。
    let session = Session {
        cost: None,
        tokens: Some(0.0),
        partial: false,
        ..Session::default()
    };
    let line = status_text(None, &session, 80, true);
    assert_eq!(
        line.lines().next().unwrap(),
        "会话 \x1b[36m$--\x1b[0m · \x1b[35m0\x1b[0m Token"
    );
    let session = Session {
        cost: Some(0.53),
        tokens: Some(10_450_000.0),
        partial: false,
        ..Session::default()
    };
    let line = status_text(None, &session, 80, true);
    assert_eq!(
        line.lines().next().unwrap(),
        "会话 \x1b[36m$0.53\x1b[0m · \x1b[35m10.45M\x1b[0m Token"
    );
}
#[test]
fn layout_never_dims_stretches_or_drops_session_money() {
    let mut r = snapshot(4.0, 1.54, at());
    r["estimate"] = json!({"available":true,"usd":51.19});
    let session = Session {
        cost: Some(0.85),
        tokens: Some(3_721_843.0),
        partial: false,
        ..Session::default()
    };
    for w in [36, 60, 80, 120, 220] {
        let line = status_text(Some(&r), &session, w, true);
        assert!(line.lines().next().unwrap().contains("会话"));
        assert!(line.contains("\x1b[36m$0.85\x1b[0m"));
        assert!(line.contains("\x1b[35m3.72M\x1b[0m"));
        let plain = strip_ansi(&line);
        assert_eq!(plain, status_text(Some(&r), &session, w, false));
        assert!(plain.contains("4%"));
        assert!(plain.contains("已用 $1.54 · 12K Token"));
        assert!(!line.contains('|'));
        assert!(!line.contains("\x1b[2m"));
        assert!(line.lines().all(|l| visible_width(l) <= w));
        assert!(line.lines().count() <= 4);
    }
    // The usage bar only appears when the two-row grid fits with room to spare.
    assert!(status_text(Some(&r), &session, 120, true).contains('░'));
    assert!(!status_text(Some(&r), &session, 60, true).contains('░'));
    assert_eq!(
        status_text(Some(&r), &session, 120, true),
        status_text(Some(&r), &session, 220, true)
    );
    r["quota"]["used_percent"] = json!(96);
    let hot = status_text(Some(&r), &session, 80, true);
    assert!(hot.contains("\x1b[31m"));
    assert!(hot.contains("96%"));
    r["stale"] = json!(true);
    assert!(status_text(Some(&r), &session, 80, false).contains("旧数据"));
}
#[test]
fn html_script_injection_is_escaped() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("report.html");
    write_html(&json!({"test":"</script><script>alert(1)</script>"}), &path).unwrap();
    let raw = fs::read_to_string(path).unwrap();
    assert!(!raw.contains("</script><script>alert(1)"));
    assert!(raw.contains("\\u003c/script>"));
}
#[test]
fn statusline_colors_default_on_and_no_color_disables() {
    let tmp = TempDir::new().unwrap();
    let exe = env!("CARGO_BIN_EXE_grok-budget");
    let payload = b"{\"cost\":{\"total_cost_usd\":0.53},\"context_window\":{\"session_input_tokens\":100,\"session_output_tokens\":20},\"session_id\":\"x\"}";
    let run = |no_color: bool| {
        let mut cmd = std::process::Command::new(exe);
        cmd.args(["--statusline", "--offline", "--grok-home"])
            .arg(tmp.path())
            .env("COLUMNS", "120")
            .env_remove("NO_COLOR")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        if no_color {
            cmd.env("NO_COLOR", "1");
        }
        let mut child = cmd.spawn().unwrap();
        use std::io::Write;
        child.stdin.as_mut().unwrap().write_all(payload).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let colored = run(false);
    assert!(colored.contains("\x1b[36m$0.53"), "{colored:?}");
    assert!(colored.contains("\x1b[35m120"), "{colored:?}");
    let plain = run(true);
    assert!(!plain.contains('\u{1b}'), "{plain:?}");
    assert!(plain.contains("会话 $0.53 · 120 Token"), "{plain}");
    assert_eq!(strip_ansi(&colored), plain);
}

#[test]
fn colors_do_not_cross_cells_or_steal_usage_warning() {
    for t in [0.0, 1.0, 9.0, 96.0, 12000.0] {
        for pct in [0.0, 80.0, 96.0] {
            let mut r = snapshot(pct, 1.54, at());
            r["estimate"] = json!({"available":true,"usd":51.19});
            let session = Session {
                cost: Some(0.5),
                tokens: Some(t),
                partial: false,
                ..Session::default()
            };
            for w in [36, 60, 80, 120] {
                let text = status_text(Some(&r), &session, w, true);
                assert!(text.contains("\x1b[35m12K\x1b[0m"), "{text:?}");
                let code = if pct >= 95.0 {
                    31
                } else if pct >= 80.0 {
                    33
                } else {
                    32
                };
                let expected = format!("\x1b[{code}m");
                assert!(text.contains(&expected), "{text:?}");
                assert_eq!(strip_ansi(&text), status_text(Some(&r), &session, w, false));
            }
        }
    }
    for w in 1..130 {
        let r = snapshot(96.0, 1.54, at());
        let session = Session {
            cost: Some(0.5),
            tokens: Some(1.0),
            partial: true,
            ..Session::default()
        };
        assert_eq!(
            strip_ansi(&status_text(Some(&r), &session, w, true)),
            status_text(Some(&r), &session, w, false)
        );
    }
}

#[test]
fn exhaustion_uses_total_account_usage_including_chat() {
    let mut current = snapshot(95.0, 5.0, at());
    current["quota"]["products"] =
        json!([{"product":"GrokBuild","used_percent":5},{"product":"GrokChat","used_percent":90}]);
    let mut old = snapshot(94.0, 4.0, at() - 3600.0);
    old["quota"]["products"] =
        json!([{"product":"GrokBuild","used_percent":4},{"product":"GrokChat","used_percent":90}]);
    let prediction = extrapolate(&current, &[old]);
    assert_eq!(prediction["hours_to_full"], 5.0);
    assert_eq!(prediction["basis"], "账户总占比");
}

#[test]
fn active_polling_has_jitter_tolerance_then_returns_to_idle() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    signal_at(tmp.path(), &dir, Some("start"), true, at()).unwrap();
    collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    let r = collect_at(tmp.path(), &dir, Options::default(), at() + 9.9, |_| {
        normalize_billing(&billing(2.0))
    })
    .unwrap();
    assert_eq!(r["sampled_at"], at() + 9.9);
    collect_at(tmp.path(), &dir, Options::default(), at() + 70.0, |_| {
        normalize_billing(&billing(3.0))
    })
    .unwrap();
    let r = collect_at(tmp.path(), &dir, Options::default(), at() + 80.0, |_| {
        panic!("idle must use cache")
    })
    .unwrap();
    assert_eq!(r["sampled_at"], at() + 70.0);
    let r = collect_at(tmp.path(), &dir, Options::default(), at() + 129.9, |_| {
        normalize_billing(&billing(4.0))
    })
    .unwrap();
    assert_eq!(r["sampled_at"], at() + 129.9);
}

#[test]
fn stop_delays_sampling_and_rechecks_unchanged_quota_once() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    signal_at(tmp.path(), &dir, Some("stop"), true, at() + 6.0).unwrap();
    collect_at(tmp.path(), &dir, Options::default(), at() + 7.0, |_| {
        panic!("wait for billing settlement")
    })
    .unwrap();
    let r = collect_at(tmp.path(), &dir, Options::default(), at() + 8.0, |_| {
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    assert_eq!(r["sampled_at"], at() + 8.0);
    let r = collect_at(tmp.path(), &dir, Options::default(), at() + 18.0, |_| {
        normalize_billing(&billing(2.0))
    })
    .unwrap();
    assert_eq!(r["quota"]["used_percent"], 2.0);
    let db = connect(&dir).unwrap();
    let follow: f64 = db
        .query_row("SELECT follow_due FROM refresh_schedule", [], |r| r.get(0))
        .unwrap();
    assert_eq!(follow, 0.0);
}

#[test]
fn force_still_merges_an_inflight_request_and_does_not_hold_write_lock() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    let r = collect_at(
        tmp.path(),
        &dir,
        Options {
            force: true,
            ..Default::default()
        },
        at() + 1.0,
        |_| {
            let other = collect_at(
                tmp.path(),
                &dir,
                Options {
                    force: true,
                    ..Default::default()
                },
                at() + 2.0,
                |_| panic!("duplicate HTTP"),
            )
            .unwrap();
            assert_eq!(other["sampled_at"], at());
            normalize_billing(&billing(2.0))
        },
    )
    .unwrap();
    assert_eq!(r["quota"]["used_percent"], 2.0);
}

#[test]
fn failures_back_off_and_manual_refresh_respects_retry_after() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    collect_at(tmp.path(), &dir, Options::default(), at() + 60.0, |_| {
        anyhow::bail!("network unavailable")
    })
    .unwrap();
    collect_at(tmp.path(), &dir, Options::default(), at() + 65.0, |_| {
        panic!("retry too early")
    })
    .unwrap();
    collect_at(tmp.path(), &dir, Options::default(), at() + 70.0, |_| {
        anyhow::bail!("network unavailable")
    })
    .unwrap();
    collect_at(tmp.path(), &dir, Options::default(), at() + 89.0, |_| {
        panic!("second failure must wait 20 seconds")
    })
    .unwrap();
    collect_at(
        tmp.path(),
        &dir,
        Options {
            force: true,
            ..Default::default()
        },
        at() + 89.0,
        |_| {
            Err(BillingFailure {
                message: "rate limited",
                retry_seconds: 120.0,
                rate_limited: true,
            }
            .into())
        },
    )
    .unwrap();
    collect_at(
        tmp.path(),
        &dir,
        Options {
            force: true,
            ..Default::default()
        },
        at() + 100.0,
        |_| panic!("force cannot bypass server Retry-After"),
    )
    .unwrap();
    let r = collect_at(tmp.path(), &dir, Options::default(), at() + 209.0, |_| {
        normalize_billing(&billing(2.0))
    })
    .unwrap();
    assert_eq!(r["stale"], false);
    assert_eq!(retry_after(Some("120"), at()), 120.0);
    assert_eq!(
        retry_after(Some("Tue, 29 Sep 2026 02:02:00 GMT"), at()),
        120.0
    );
    assert_eq!(retry_after(Some("garbage"), at()), 60.0);
}

#[test]
fn local_cache_invalidates_on_ledger_change_without_changing_samples() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    let path = ledger(tmp.path(), "a", vec![turn()]);
    let mut r = collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    let mut contents = read_json(&path).unwrap();
    let mut next = turn();
    next["endedAt"] = json!(iso(at() + 0.1));
    contents["turns"].as_array_mut().unwrap().push(next);
    fs::write(path, contents.to_string()).unwrap();
    refresh_local_cached(tmp.path(), &dir, &mut r, at() + 0.2).unwrap();
    assert_eq!(r["live_local"]["totalTokens"], 220.0);
    assert_eq!(r["local"]["totalTokens"], 110.0);
    let old = collect_at(
        tmp.path(),
        &dir,
        Options {
            offline: true,
            ..Default::default()
        },
        at() + 0.3,
        |_| panic!(),
    )
    .unwrap();
    assert!(old["live_local"].is_null());
    assert_eq!(old["local"]["totalTokens"], 110.0);
}

#[test]
fn offline_hook_does_not_signal_background_network_requests() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_grok-budget"))
        .args(["--hook", "--event", "stop", "--offline", "--grok-home"])
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!tmp.path().join("grok-budget/history.sqlite3").exists());
}

#[test]
fn account_period_rollover_is_not_delayed_by_idle_cache() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    let end = timestamp(&json!(END)).unwrap();
    collect_at(tmp.path(), &dir, Options::default(), end - 10.0, |_| {
        normalize_billing(&billing(95.0))
    })
    .unwrap();
    let r = collect_at(tmp.path(), &dir, Options::default(), end + 0.1, |_| {
        let mut b = billing(0.0);
        b["config"]["currentPeriod"]["start"] = json!(END);
        b["config"]["currentPeriod"]["end"] = json!("2026-10-12T16:08:48+00:00");
        normalize_billing(&b)
    })
    .unwrap();
    assert_eq!(r["quota"]["used_percent"], 0.0);
    assert_eq!(r["stale"], false);
}

#[test]
fn missing_history_cost_is_not_treated_as_zero() {
    let current = snapshot(10.0, 5.0, at());
    let mut old = snapshot(2.0, 1.0, at() - 1000.0);
    old["local"].as_object_mut().unwrap().remove("cost_usd");
    let estimate = estimate(&current, &[old]);
    assert_eq!(estimate["method"], "period");
    assert_eq!(estimate["usd"], 50.0);
}

#[test]
fn damaged_snapshot_does_not_hide_the_last_good_sample_or_block_sampling() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        normalize_billing(&billing(4.0))
    })
    .unwrap();
    let (_, account) = credentials(tmp.path()).unwrap();
    let db = connect(&dir).unwrap();
    db.execute(
        "INSERT INTO snapshots(account,sampled,payload) VALUES (?,?,?)",
        rusqlite::params![account, at() + 1.0, "{"],
    )
    .unwrap();
    drop(db);
    let cached = collect_at(
        tmp.path(),
        &dir,
        Options {
            offline: true,
            ..Default::default()
        },
        at() + 2.0,
        |_| panic!(),
    )
    .unwrap();
    assert_eq!(cached["quota"]["used_percent"], 4.0);
    let fresh = collect_at(tmp.path(), &dir, Options::default(), at() + 120.0, |_| {
        normalize_billing(&billing(8.0))
    })
    .unwrap();
    assert_eq!(fresh["quota"]["used_percent"], 8.0);
    assert_eq!(fresh["stale"], false);
}

#[test]
fn stored_sampling_error_does_not_keep_the_cause_chain() {
    let tmp = TempDir::new().unwrap();
    auth(tmp.path(), "a");
    let dir = tmp.path().join("data");
    collect_at(tmp.path(), &dir, Options::default(), at(), |_| {
        normalize_billing(&billing(1.0))
    })
    .unwrap();
    let r = collect_at(tmp.path(), &dir, Options::default(), at() + 120.0, |_| {
        Err(anyhow::anyhow!("SECRET_BODY").context("额度响应不是有效 JSON"))
    })
    .unwrap();
    assert_eq!(r["error"], "额度响应不是有效 JSON");
    assert!(!r["error"].as_str().unwrap().contains("SECRET_BODY"));
}
