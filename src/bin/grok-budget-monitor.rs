// Task Scheduler owns this process directly. No console window or shell child
// survives when the task is stopped, and native exit codes reach the scheduler.
#![cfg_attr(windows, windows_subsystem = "windows")]

use clap::Parser;
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(version, about = "Grok Budget background monitor")]
struct Args {
    #[arg(long)]
    grok_home: PathBuf,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let dir = args.grok_home.join("grok-budget");
    match grok_budget::storage::run_monitor(&args.grok_home, &dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
