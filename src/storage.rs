use crate::{data, *};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

pub fn connect(dir: &Path) -> Result<Connection> {
    fs::create_dir_all(dir)?;
    let db = Connection::open(dir.join("history.sqlite3"))?;
    db.busy_timeout(Duration::from_secs(1))?;
    // The status line reads every second while the monitor writes samples;
    // WAL lets those never block each other and drops per-write fsyncs.
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS snapshots (id INTEGER PRIMARY KEY, account TEXT NOT NULL, sampled REAL NOT NULL, payload TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS snapshots_account ON snapshots(account,sampled);
        CREATE TABLE IF NOT EXISTS attempts (account TEXT PRIMARY KEY, attempted REAL NOT NULL, error TEXT);
        CREATE TABLE IF NOT EXISTS refresh_schedule (account TEXT PRIMARY KEY,
        active_until REAL NOT NULL DEFAULT 0, next_due REAL NOT NULL DEFAULT 0,
        retry_until REAL NOT NULL DEFAULT 0, rate_limit_until REAL NOT NULL DEFAULT 0, inflight_until REAL NOT NULL DEFAULT 0,
        failures INTEGER NOT NULL DEFAULT 0, event_due REAL NOT NULL DEFAULT 0,
        follow_due REAL NOT NULL DEFAULT 0, baseline TEXT, heartbeat_until REAL NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS local_cache (account TEXT PRIMARY KEY, fingerprint TEXT NOT NULL,
        period_start REAL NOT NULL, sampled REAL NOT NULL, payload TEXT NOT NULL);")?;
    Ok(db)
}
fn cached(db: &Connection, account: &str) -> Result<Option<Value>> {
    // One damaged row must not hide the last good sample or block the next fetch.
    let mut stmt =
        db.prepare("SELECT payload FROM snapshots WHERE account=? ORDER BY id DESC LIMIT 8")?;
    let rows = stmt.query_map([account], |r| r.get::<_, String>(0))?;
    for row in rows {
        if let Ok(value) = serde_json::from_str(&row?) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}
fn attempt(db: &Connection, account: &str) -> Result<Option<(f64, Option<String>)>> {
    Ok(db
        .query_row(
            "SELECT attempted,error FROM attempts WHERE account=?",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}
pub fn annotate(mut r: Value, error: Option<&str>, at: f64) -> Value {
    let age = (at - n(&r["sampled_at"])).max(0.0).round();
    r["cache_age_seconds"] = json!(age);
    r["stale"] = json!(
        error.is_some() || age > 180.0 || timestamp(&r["quota"]["period_end"]).unwrap_or(0.0) <= at
    );
    if let Some(error) = error {
        r["error"] = json!(error);
    }
    r
}
#[derive(Default, Clone, Copy)]
pub struct Options {
    pub force: bool,
    pub offline: bool,
    pub cache_only: bool,
}
pub fn collect(home: &Path, dir: &Path, opts: Options) -> Result<Value> {
    collect_at(home, dir, opts, now(), data::fetch_billing)
}

pub fn collect_at<F>(home: &Path, dir: &Path, opts: Options, at: f64, fetch: F) -> Result<Value>
where
    F: FnOnce(&str) -> Result<Value>,
{
    let (key, account) = credentials(home)?;
    let mut db = connect(dir)?;
    let previous = cached(&db, &account)?;
    if opts.offline || opts.cache_only {
        let error = attempt(&db, &account)?.and_then(|a| a.1);
        return previous
            .map(|r| annotate(r, error.as_deref(), at))
            .context("还没有额度缓存，请先运行 grok-budget.exe --refresh。");
    }
    // Reserve the attempt under a short lock; neither HTTP nor ledger I/O holds it.
    let reserve = (|| -> Result<Option<Value>> {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let a = attempt(&tx, &account)?;
        ensure_schedule(&tx, &account)?;
        let schedule = schedule(&tx, &account)?;
        if !ready(&schedule, a.as_ref().map(|v| v.0), at, opts.force) {
            let prev = cached(&tx, &account)?;
            let error = a.and_then(|a| a.1);
            return prev
                .map(|r| Some(annotate(r, error.as_deref(), at)))
                .context("首次采样仍在等待，请稍后再试。");
        }
        tx.execute(
            "INSERT INTO attempts VALUES (?,?,NULL) ON CONFLICT(account) DO UPDATE SET attempted=excluded.attempted",
            params![account, at],
        )?;
        tx.execute("UPDATE refresh_schedule SET inflight_until=?, event_due=CASE WHEN event_due<=? THEN 0 ELSE event_due END,
            follow_due=CASE WHEN follow_due<=? THEN 0 ELSE follow_due END WHERE account=?",
            params![at + 30.0, at, at, account])?;
        tx.commit()?;
        Ok(None)
    })();
    match reserve {
        Ok(Some(r)) => return Ok(r),
        Ok(None) => {}
        Err(e) => {
            return match previous {
                Some(r) => Ok(annotate(r, Some("其他采样进程正在更新，暂用缓存。"), at)),
                None => Err(e),
            };
        }
    }
    let fresh = (|| -> Result<Value> {
        let quota = fetch(&key)?;
        let start = timestamp(&quota["period_start"])?;
        let end = timestamp(&quota["period_end"])?;
        if !(start <= at && at < end) {
            bail!("服务器返回的周期未覆盖当前时间；本次不记录估算样本。");
        }
        let usage = cached_local(home, &db, &account, start, at.min(end))?;
        let mut report = json!({"schema_version":1,"account":account,"sampled_at":at,"sampled_at_iso":iso(at),"source":ENDPOINT,"quota":quota,"local":usage});
        // Same-period history is append-only and can grow to tens of thousands of
        // rows; parse it before taking the write lock so readers never wait on it.
        let history = {
            let mut stmt = db.prepare(
                "SELECT payload FROM snapshots WHERE account=? AND sampled>=? ORDER BY id",
            )?;
            let rows = stmt.query_map(params![account, start], |r| r.get::<_, String>(0))?;
            let mut history = Vec::new();
            for row in rows {
                if let Ok(value) = serde_json::from_str::<Value>(&row?) {
                    history.push(value);
                }
            }
            history
        };
        report["estimate"] = data::estimate(&report, &history);
        report["projection"] = data::extrapolate(&report, &history);
        report["sample_count"] = json!(history.len() + 1);
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A newer reservation owns the row now; an expired worker only reads.
        // Every snapshot insert also rewrites `attempts`, so this check alone
        // guarantees no newer snapshot exists for this account.
        if attempt(&tx, &account)?.is_some_and(|a| a.0 != at) {
            return cached(&tx, &account)?
                .map(|r| annotate(r, None, at))
                .context("其他采样进程正在更新。");
        }
        tx.execute(
            "INSERT INTO snapshots(account,sampled,payload) VALUES (?,?,?)",
            params![account, at, report.to_string()],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO attempts VALUES (?,?,NULL)",
            params![account, at],
        )?;
        let state = schedule(&tx, &account)?;
        let interval = if state.active_until > at { 9.5 } else { 59.5 };
        tx.execute(
            "UPDATE refresh_schedule SET inflight_until=0, failures=0, retry_until=0, rate_limit_until=0, next_due=?,
            follow_due=CASE WHEN baseline=? THEN follow_due ELSE 0 END WHERE account=?",
            params![(at + interval).min(end), quota.to_string(), account],
        )?;
        tx.execute(
            "DELETE FROM snapshots WHERE sampled<?",
            [at - 90.0 * 86400.0],
        )?;
        tx.commit()?;
        report["live_local"] = report["local"].clone();
        report["local_refreshed_at"] = json!(iso(at));
        Ok(annotate(report, None, at))
    })();
    match fresh {
        Ok(r) => Ok(r),
        Err(e) => {
            // Store only the outer message. anyhow's full chain can quote response text.
            let message = e
                .chain()
                .next()
                .map_or_else(|| e.to_string(), |err| err.to_string());
            let billing = e.downcast_ref::<data::BillingFailure>();
            let _ = (|| -> Result<()> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if attempt(&tx, &account)?.is_some_and(|a| a.0 == at) {
                    let state = schedule(&tx, &account)?;
                    let failures = state.failures.saturating_add(1).min(10);
                    let delay = billing
                        .map(|v| v.retry_seconds)
                        .unwrap_or_else(|| (10.0 * 2f64.powi(failures - 1)).min(300.0));
                    let rate_limit = billing.is_some_and(|v| v.rate_limited);
                    tx.execute(
                        "UPDATE attempts SET error=? WHERE account=? AND attempted=?",
                        params![message, account, at],
                    )?;
                    tx.execute("UPDATE refresh_schedule SET inflight_until=0, failures=?, retry_until=?, rate_limit_until=? WHERE account=?",
                        params![failures, at + delay, if rate_limit { at + delay } else { 0.0 }, account])?;
                }
                tx.commit()?;
                Ok(())
            })();
            previous.map(|r| annotate(r, Some(&message), at)).ok_or(e)
        }
    }
}

#[derive(Debug, Default)]
struct Schedule {
    active_until: f64,
    next_due: f64,
    retry_until: f64,
    rate_limit_until: f64,
    inflight_until: f64,
    failures: i32,
    event_due: f64,
    follow_due: f64,
    heartbeat_until: f64,
}
/// Create the account row so later UPDATEs have something to hit. Writers only.
fn ensure_schedule(db: &Connection, account: &str) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO refresh_schedule(account) VALUES (?)",
        [account],
    )?;
    Ok(())
}
/// Read-only: a missing row is the all-zero default, so the monitor's 500 ms
/// poll never takes a write lock.
fn schedule(db: &Connection, account: &str) -> Result<Schedule> {
    Ok(db.query_row("SELECT active_until,next_due,retry_until,inflight_until,failures,event_due,follow_due,heartbeat_until,rate_limit_until FROM refresh_schedule WHERE account=?", [account], |r| Ok(Schedule {
        active_until:r.get(0)?, next_due:r.get(1)?, retry_until:r.get(2)?, inflight_until:r.get(3)?,
        failures:r.get(4)?, event_due:r.get(5)?, follow_due:r.get(6)?, heartbeat_until:r.get(7)?, rate_limit_until:r.get(8)?,
    })).optional()?.unwrap_or_default())
}
fn due_at(state: &Schedule, attempted: Option<f64>) -> f64 {
    let mut due = state.next_due;
    for event in [state.event_due, state.follow_due] {
        if event > 0.0 {
            due = due.min(event);
        }
    }
    // retry_until and inflight_until are rejected in ready(); folding them in
    // here cannot change the result.
    due.max(attempted.map(|a| a + 5.0).unwrap_or(0.0))
}
fn ready(state: &Schedule, attempted: Option<f64>, at: f64, force: bool) -> bool {
    if state.inflight_until > at
        || state.rate_limit_until > at
        || (!force && state.retry_until > at)
    {
        return false;
    }
    force || due_at(state, attempted) <= at
}

/// Signal activity without performing HTTP; account-level state coalesces all sessions.
pub fn signal_at(
    home: &Path,
    dir: &Path,
    event: Option<&str>,
    active: bool,
    at: f64,
) -> Result<()> {
    let (_, account) = credentials(home)?;
    let mut db = connect(dir)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_schedule(&tx, &account)?;
    tx.execute(
        "UPDATE refresh_schedule SET heartbeat_until=MAX(heartbeat_until,?) WHERE account=?",
        params![at + 20.0, account],
    )?;
    if active || event.is_some() {
        let last = attempt(&tx, &account)?.map(|a| a.0).unwrap_or(0.0);
        tx.execute(
            "UPDATE refresh_schedule SET active_until=?, next_due=MIN(next_due,?) WHERE account=?",
            params![at + 60.0, last + 9.5, account],
        )?;
    }
    if let Some(event) = event {
        let baseline = cached(&tx, &account)?
            .map(|r| r["quota"].to_string())
            .unwrap_or_default();
        let delay = if event == "stop" { 2.0 } else { 0.0 };
        tx.execute(
            "UPDATE refresh_schedule SET event_due=?, follow_due=?, baseline=? WHERE account=?",
            params![
                at + delay,
                if event == "stop" { at + 12.0 } else { 0.0 },
                baseline,
                account
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// The monitor is launched independently by Windows Task Scheduler. Grok kills
/// subprocess trees of status commands, so UI/hook processes must only signal it.
pub fn request_background(
    home: &Path,
    dir: &Path,
    event: Option<&str>,
    active: bool,
) -> Result<()> {
    signal_at(home, dir, event, active, now())
}

pub fn monitor_tick(home: &Path, dir: &Path, db: &Connection) -> Result<()> {
    let (_, account) = credentials(home)?;
    let state = schedule(db, &account)?;
    let attempted = attempt(db, &account)?.map(|a| a.0);
    let at = now();
    if state.heartbeat_until > at && ready(&state, attempted, at, false) {
        let _ = collect(home, dir, Options::default());
    }
    Ok(())
}

pub fn run_monitor(home: &Path, dir: &Path) -> Result<()> {
    // One read connection for the poll; collect() opens its own for the write.
    let db = connect(dir)?;
    loop {
        let _ = monitor_tick(home, dir, &db);
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn cached_local(home: &Path, db: &Connection, account: &str, start: f64, at: f64) -> Result<Value> {
    use sha2::{Digest, Sha256};
    use std::time::UNIX_EPOCH;
    let mut digest = Sha256::new();
    for path in data::ledgers(home) {
        digest.update(path.to_string_lossy().as_bytes());
        if let Ok(m) = fs::metadata(&path) {
            digest.update(m.len().to_le_bytes());
            let modified = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            digest.update(modified.to_le_bytes());
        }
    }
    let fingerprint = format!("{:x}", digest.finalize());
    let hit: Option<String> = db.query_row("SELECT payload FROM local_cache WHERE account=? AND fingerprint=? AND period_start=? AND sampled<=? AND sampled>?",
        params![account, fingerprint, start, at, at - 10.0], |r| r.get(0)).optional()?;
    if let Some(hit) = hit {
        return Ok(serde_json::from_str(&hit)?);
    }
    let usage = data::local_usage(home, start, at);
    // Cache aggregate counts only, never raw ledgers, paths, messages or credentials.
    db.execute(
        "INSERT OR REPLACE INTO local_cache VALUES (?,?,?,?,?)",
        params![account, fingerprint, start, at, usage.to_string()],
    )?;
    Ok(usage)
}

pub fn refresh_local_cached(home: &Path, dir: &Path, report: &mut Value, at: f64) -> Result<()> {
    let start = timestamp(&report["quota"]["period_start"])?;
    let end = timestamp(&report["quota"]["period_end"])?;
    if start <= at && at < end {
        // The report was just loaded for the current credentials; its account
        // is the cache key, so there is nothing to re-derive from auth.json.
        let account = s(&report["account"]).to_string();
        report["live_local"] = cached_local(home, &connect(dir)?, &account, start, at)?;
        report["local_refreshed_at"] = json!(iso(at));
    }
    Ok(())
}
