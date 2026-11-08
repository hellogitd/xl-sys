//! `xl-taskd-mcp` —— MCP server 入口。
//!
//! 协议走 stdout，日志走 stderr（MCP 规定 stdout 只能有协议消息）。

use std::env;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use taskd_mcp::McpServer;
use taskd_store::Store;
use taskd_undo::SnapshotStore;

fn main() -> ExitCode {
    let db = env::var("XLTASKD_DB").unwrap_or_else(|_| "taskd.db".to_string());
    let snap_dir =
        env::var("XLTASKD_SNAPSHOTS").unwrap_or_else(|_| "taskd-snapshots".to_string());
    eprintln!("[xl-taskd-mcp] 数据库：{db}");
    eprintln!("[xl-taskd-mcp] 快照目录：{snap_dir}");

    let store = match Store::open(&db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[xl-taskd-mcp] 打开数据库失败：{e}");
            return ExitCode::FAILURE;
        }
    };
    let snapshots = match SnapshotStore::new(&snap_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[xl-taskd-mcp] 初始化快照目录失败：{e}");
            return ExitCode::FAILURE;
        }
    };

    let mut server = McpServer::new(store, snapshots);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    eprintln!("[xl-taskd-mcp] 就绪，等待 MCP 消息");

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = server.handle_line(&line) {
            if writeln!(out, "{resp}").is_err() {
                break;
            }
            if out.flush().is_err() {
                break;
            }
        }
    }

    eprintln!("[xl-taskd-mcp] 退出");
    ExitCode::SUCCESS
}
