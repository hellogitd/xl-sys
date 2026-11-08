//! `xl-taskd` —— S0.1 的最小可用 CLI。
//!
//! 目标：证明「任务 / 工作单元 / 步骤」这套模型能记录并回放真实操作历史。
//! 还没有：委托检查、撤销引擎、溯源。那些是 S0.2–S0.4。

use std::env;
use std::process::ExitCode;

use taskd_core::*;
use taskd_store::Store;

const DEFAULT_DB: &str = "taskd.db";

fn db_path() -> String {
    env::var("XLTASKD_DB").unwrap_or_else(|_| DEFAULT_DB.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        help();
        return ExitCode::SUCCESS;
    }

    let result = match args[0].as_str() {
        "begin" => cmd_begin(&args[1..]),
        "step" => cmd_step(&args[1..]),
        "status" => cmd_status(&args[1..]),
        "list" => cmd_list(),
        "done" => cmd_done(&args[1..]),
        "unit-begin" => cmd_unit_begin(&args[1..]),
        "unit-end" => cmd_unit_end(&args[1..]),
        "help" | "--help" | "-h" => {
            help();
            Ok(())
        }
        other => Err(format!("未知命令：{other}\n运行 `xl-taskd help` 查看用法")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误：{e}");
            ExitCode::FAILURE
        }
    }
}

fn help() {
    println!(
        r#"xl-taskd —— XL-OS 任务运行时（S0.1）

用法：
  xl-taskd begin <目标>                    创建一个任务
  xl-taskd step <任务编号> <意图...>        记录一步操作
  xl-taskd status <任务编号>               查看任务历史
  xl-taskd list                           列出所有任务
  xl-taskd done <任务编号>                 标记任务完成
  xl-taskd unit-begin <任务编号> [类型]     开始一个工作单元（attempt/change/release）
  xl-taskd unit-end <单元编号> [结果]       结束工作单元（succeeded/failed/abandoned）

环境变量：
  XLTASKD_DB    数据库路径，默认 ./taskd.db

示例：
  xl-taskd begin 整理下载文件夹里的截图
  xl-taskd step task_01J... 列出下载目录中的 png 文件
  xl-taskd status task_01J..."#
    );
}

fn open_store() -> Result<Store, String> {
    Store::open(&db_path()).map_err(|e| format!("打开数据库失败：{e}"))
}

// ---------------------------------------------------------------- begin

fn cmd_begin(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("缺少目标。用法：xl-taskd begin <目标>".into());
    }
    let goal = args.join(" ");
    let store = open_store()?;
    let task = Task::new(&goal);
    store.create_task(&task).map_err(|e| e.to_string())?;

    println!("任务已创建");
    println!("  编号  {}", task.id);
    println!("  目标  {}", task.goal);
    println!("  状态  {}", state_label(task.state));
    Ok(())
}

// ---------------------------------------------------------------- step

fn cmd_step(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("用法：xl-taskd step <任务编号> <意图...>".into());
    }
    let task_id = &args[0];
    let intent = args[1..].join(" ");

    let store = open_store()?;
    let mut task = store
        .get_task(task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("任务不存在：{task_id}"))?;

    // 首次记录步骤时自动进入运行中
    if task.state == TaskState::Pending {
        task.transition_to(TaskState::Running)
            .map_err(|e| e.to_string())?;
        store
            .update_task_state(&task.id, TaskState::Running)
            .map_err(|e| e.to_string())?;
    }

    let idx = store.next_step_index(&task.id).map_err(|e| e.to_string())?;
    let mut step = Step::new(&task.id, idx, StepKind::ToolCall, &intent);
    step.finish(StepResult::Ok { payload: None });
    store.append_step(&step).map_err(|e| e.to_string())?;

    println!("已记录第 {idx} 步");
    println!("  意图  {}", step.intent);
    println!("  风险  {}（{}）", risk_label(step.risk), risk_desc(step.risk));
    Ok(())
}

// ---------------------------------------------------------------- status

fn cmd_status(args: &[String]) -> Result<(), String> {
    let task_id = args.first().ok_or("用法：xl-taskd status <任务编号>")?;
    let store = open_store()?;
    let task = store
        .get_task(task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("任务不存在：{task_id}"))?;

    let units = store.list_units(&task.id).map_err(|e| e.to_string())?;
    let steps = store.list_steps(&task.id).map_err(|e| e.to_string())?;

    println!("任务 {}", task.id);
    println!("  目标  {}", task.goal);
    println!("  状态  {}", state_label(task.state));
    println!("  创建  {}", fmt_time(task.created_at));

    if !units.is_empty() {
        println!("  工作单元  {} 个", units.len());
        for u in &units {
            println!(
                "    {}  {}  {}",
                u.id,
                unit_kind_label(u.kind),
                outcome_label(u.outcome)
            );
        }
    }

    println!("  步骤  {} 步", steps.len());
    for s in &steps {
        let unit = s
            .unit_id
            .as_ref()
            .map(|u| format!(" [{}]", short(u)))
            .unwrap_or_default();
        println!(
            "    {:<3} {}  {}{}  {}",
            s.index,
            risk_label(s.risk),
            s.intent,
            unit,
            fmt_time(s.started_at)
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- list

fn cmd_list() -> Result<(), String> {
    let store = open_store()?;
    let tasks = store.list_tasks().map_err(|e| e.to_string())?;
    if tasks.is_empty() {
        println!("还没有任务。用 `xl-taskd begin <目标>` 创建。");
        return Ok(());
    }
    println!("{:<28} {:<10} {}", "编号", "状态", "目标");
    for t in &tasks {
        println!(
            "{:<28} {:<10} {}",
            t.id,
            state_label(t.state),
            t.goal
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- done

fn cmd_done(args: &[String]) -> Result<(), String> {
    let task_id = args.first().ok_or("用法：xl-taskd done <任务编号>")?;
    let store = open_store()?;
    let mut task = store
        .get_task(task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("任务不存在：{task_id}"))?;

    task.transition_to(TaskState::Completed)
        .map_err(|e| e.to_string())?;
    store
        .update_task_state(&task.id, TaskState::Completed)
        .map_err(|e| e.to_string())?;
    println!("任务已完成：{}", task.id);
    Ok(())
}

// ---------------------------------------------------------------- 工作单元

fn cmd_unit_begin(args: &[String]) -> Result<(), String> {
    let task_id = args.first().ok_or("用法：xl-taskd unit-begin <任务编号> [attempt|change|release]")?;
    let kind = match args.get(1).map(|s| s.as_str()) {
        None | Some("attempt") => WorkUnitKind::Attempt,
        Some("change") => WorkUnitKind::Change,
        Some("release") => WorkUnitKind::Release,
        Some(other) => return Err(format!("未知类型：{other}")),
    };

    let store = open_store()?;
    let task = store
        .get_task(task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("任务不存在：{task_id}"))?;

    let unit = WorkUnit::new(&task.id, kind);
    store.create_unit(&unit).map_err(|e| e.to_string())?;
    println!("工作单元已开始");
    println!("  编号  {}", unit.id);
    println!("  类型  {}", unit_kind_label(kind));
    Ok(())
}

fn cmd_unit_end(args: &[String]) -> Result<(), String> {
    let unit_id = args.first().ok_or("用法：xl-taskd unit-end <单元编号> [succeeded|failed|abandoned]")?;
    let outcome = match args.get(1).map(|s| s.as_str()) {
        None | Some("succeeded") => Outcome::Succeeded,
        Some("failed") => Outcome::Failed,
        Some("abandoned") => Outcome::Abandoned,
        Some(other) => return Err(format!("未知结果：{other}")),
    };
    let store = open_store()?;
    store
        .finish_unit(unit_id, outcome)
        .map_err(|e| e.to_string())?;
    println!("工作单元已结束：{} → {}", short(unit_id), outcome_label(outcome));
    Ok(())
}

// ---------------------------------------------------------------- 格式化

fn short(id: &str) -> String {
    match id.rsplit_once('_') {
        Some((_, tail)) if tail.len() > 8 => tail[..8].to_string(),
        _ => id.to_string(),
    }
}

fn state_label(s: TaskState) -> &'static str {
    match s {
        TaskState::Pending => "待开始",
        TaskState::Running => "运行中",
        TaskState::Paused => "已暂停",
        TaskState::Completed => "已完成",
        TaskState::Failed => "已失败",
        TaskState::RolledBack => "已回滚",
        TaskState::Deployed => "已部署",
    }
}

fn unit_kind_label(k: WorkUnitKind) -> &'static str {
    match k {
        WorkUnitKind::Attempt => "尝试",
        WorkUnitKind::Change => "变更",
        WorkUnitKind::Release => "发布",
    }
}

fn outcome_label(o: Outcome) -> &'static str {
    match o {
        Outcome::Running => "进行中",
        Outcome::Succeeded => "成功",
        Outcome::Failed => "失败",
        Outcome::Abandoned => "放弃",
    }
}

fn risk_label(r: RiskLevel) -> &'static str {
    match r {
        RiskLevel::R0 => "R0",
        RiskLevel::R1 => "R1",
        RiskLevel::R2 => "R2",
        RiskLevel::R3 => "R3",
    }
}

fn risk_desc(r: RiskLevel) -> &'static str {
    match r {
        RiskLevel::R0 => "可撤销",
        RiskLevel::R1 => "可补偿",
        RiskLevel::R2 => "不可逆",
        RiskLevel::R3 => "危险",
    }
}

/// Unix 毫秒 → `YYYY-MM-DD HH:MM`（UTC）。
fn fmt_time(ms: i64) -> String {
    let secs = ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Howard Hinnant 的 civil_from_days 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
