use anyhow::{Context, Result, bail};
use clap::Parser;
use grok_budget::{display, storage};
use serde_json::Value;
use std::{
    env,
    io::{self, Read},
    path::PathBuf,
    thread,
    time::Duration,
};

#[derive(Parser)]
#[command(
    version,
    about = "Grok Build 周额度与成本监测（Rust 原生版，不调用模型）"
)]
struct Args {
    #[arg(long)]
    grok_home: Option<PathBuf>,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    refresh: bool,
    #[arg(long)]
    offline: bool,
    #[arg(long)]
    statusline: bool,
    #[arg(long)]
    hook: bool,
    #[arg(long)]
    watch: Option<u64>,
    #[arg(long)]
    html: Option<PathBuf>,
    #[arg(long)]
    no_color: bool,
}
fn color_enabled(args: &Args) -> bool {
    !args.no_color && env::var_os("NO_COLOR").is_none()
}
fn run(args: &Args) -> Result<()> {
    if args.watch.is_some_and(|v| v < 60) {
        bail!("--watch 必须至少为 60 秒");
    }
    let home = args
        .grok_home
        .clone()
        .or_else(|| env::var_os("GROK_HOME").map(PathBuf::from))
        .or_else(|| {
            env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|v| PathBuf::from(v).join(".grok"))
        })
        .context("无法确定 Grok home")?;
    let dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| home.join("grok-budget"));
    let payload = if args.statusline {
        let mut input = String::new();
        io::stdin().take(1_048_576).read_to_string(&mut input)?;
        serde_json::from_str::<Value>(input.trim_start_matches('\u{feff}')).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    loop {
        let opts = storage::Options {
            force: args.refresh,
            offline: args.offline,
            cache_only: args.statusline && payload["trigger"] != "refresh_interval",
        };
        let mut result = storage::collect(&home, &dir, opts);
        if !args.offline
            && !args.hook
            && let Ok(report) = result.as_mut()
        {
            storage::refresh_local(&home, report, grok_budget::now())?;
        }
        if args.statusline {
            let session = display::session_usage(&home, &payload);
            let columns = env::var("COLUMNS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(80);
            println!(
                "{}",
                display::status_text(
                    result.as_ref().ok(),
                    &session,
                    columns,
                    color_enabled(args)
                )
            );
        } else {
            let report = result?;
            if let Some(path) = &args.html {
                display::write_html(&report, path)?;
            }
            if !args.hook {
                println!(
                    "{}",
                    if args.json {
                        serde_json::to_string_pretty(&report)?
                    } else {
                        display::report_text(&report)
                    }
                );
            }
        }
        match args.watch {
            Some(seconds) => thread::sleep(Duration::from_secs(seconds)),
            None => return Ok(()),
        }
    }
}
fn main() {
    let args = Args::parse();
    if let Err(e) = run(&args) {
        if args.hook {
            return;
        }
        if args.statusline {
            println!("额度暂不可用  请运行 grok-budget.cmd");
            return;
        }
        eprintln!("Grok Budget: {e}");
        std::process::exit(1);
    }
}
