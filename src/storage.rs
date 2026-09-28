use crate::{data, *};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

pub fn connect(dir: &Path) -> Result<Connection> {
    fs::create_dir_all(dir)?;
    let db = Connection::open(dir.join("history.sqlite3"))?;
    db.busy_timeout(Duration::from_secs(1))?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS snapshots (id INTEGER PRIMARY KEY, account TEXT NOT NULL, sampled REAL NOT NULL, payload TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS snapshots_account ON snapshots(account,sampled);
        CREATE TABLE IF NOT EXISTS attempts (account TEXT PRIMARY KEY, attempted REAL NOT NULL, error TEXT);")?;
    Ok(db)
}
fn cached(db: &Connection, account: &str) -> Result<Option<Value>> {
    let row: Option<String> = db
        .query_row(
            "SELECT payload FROM snapshots WHERE account=? ORDER BY id DESC LIMIT 1",
            [account],
            |r| r.get(0),
        )
        .optional()?;
    row.map(|v| serde_json::from_str(&v).context("额度缓存损坏"))
        .transpose()
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

/// Refresh local counters without pairing fresh costs with stale server quota
/// in historical estimation samples. This overlay is never persisted to SQLite.
pub fn refresh_local(home: &Path, report: &mut Value, at: f64) -> Result<()> {
    let start = timestamp(&report["quota"]["period_start"])?;
    let end = timestamp(&report["quota"]["period_end"])?;
    if start <= at && at < end {
        report["live_local"] = data::local_usage(home, start, at);
        report["local_refreshed_at"] = json!(iso(at));
    }
    Ok(())
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
            .context("还没有额度缓存，请先运行 grok-budget.cmd。");
    }
    // Reserve the attempt under a short lock; neither HTTP nor ledger I/O holds it.
    let reserve = (|| -> Result<Option<Value>> {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let a = attempt(&tx, &account)?;
        if !opts.force && a.as_ref().is_some_and(|a| at - a.0 < 60.0) {
            let prev = cached(&tx, &account)?;
            let error = a.and_then(|a| a.1);
            return prev
                .map(|r| Some(annotate(r, error.as_deref(), at)))
                .context("首次采样仍在等待，请稍后再试。");
        }
        tx.execute(
            "INSERT OR REPLACE INTO attempts VALUES (?,?,NULL)",
            params![account, at],
        )?;
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
        let usage = data::local_usage(home, start, at.min(end));
        let mut report = json!({"schema_version":1,"account":account,"sampled_at":at,"sampled_at_iso":iso(at),"source":ENDPOINT,"quota":quota,"local":usage});
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A slower refresh must not overwrite a newer snapshot.
        if let Some(newer) = cached(&tx, &account)?.filter(|r| n(&r["sampled_at"]) > at) {
            return Ok(annotate(newer, None, at));
        }
        let history = {
            let mut stmt = tx.prepare(
                "SELECT payload FROM snapshots WHERE account=? AND sampled>=? ORDER BY id",
            )?;
            let rows = stmt.query_map(params![account, start], |r| r.get::<_, String>(0))?;
            rows.map(|r| Ok(serde_json::from_str(&r?)?))
                .collect::<Result<Vec<Value>>>()?
        };
        report["estimate"] = data::estimate(&report, &history);
        report["projection"] = data::extrapolate(&report, &history);
        report["sample_count"] = json!(history.len() + 1);
        tx.execute(
            "INSERT INTO snapshots(account,sampled,payload) VALUES (?,?,?)",
            params![account, at, report.to_string()],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO attempts VALUES (?,?,NULL)",
            params![account, at],
        )?;
        tx.execute(
            "DELETE FROM snapshots WHERE sampled<?",
            [at - 90.0 * 86400.0],
        )?;
        tx.commit()?;
        Ok(annotate(report, None, at))
    })();
    match fresh {
        Ok(r) => Ok(r),
        Err(e) => {
            // Store only our sanitized error, never a request object or credentials.
            let message = e.to_string();
            let _ = db.execute(
                "UPDATE attempts SET error=? WHERE account=? AND attempted=?",
                params![message, account, at],
            );
            previous.map(|r| annotate(r, Some(&message), at)).ok_or(e)
        }
    }
}
