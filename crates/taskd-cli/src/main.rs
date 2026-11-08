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
        "request" => cmd_request(&args[1..]),
        "approve" => cmd_approve(&args[1..]),
        "delegations" => cmd_delegations(&args[1..]),
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
         [--target <路径>] [--action <动作>]   提供后先做委托检查
  xl-taskd status <任务编号>               查看任务历史
  xl-taskd list                           列出所有任务
  xl-taskd done <任务编号>                 标记任务完成
  xl-taskd unit-begin <任务编号> [类型]     开始一个工作单元（attempt/change/release）
  xl-taskd unit-end <单元编号> [结果]       结束工作单元（succeeded/failed/abandoned）
  xl-taskd request <任务编号> <范围> <动作...>  申请委托（需人类批准）
  xl-taskd approve <委托编号>              批准委托（人类动作）
  xl-taskd delegations <任务编号>           查看委托状态

环境变量：
  XLTASKD_DB    数据库路径，默认 ./taskd.db

示例：
  xl-taskd begin 整理下载文件夹里的截图
  xl-taskd step task_01J... "移动文件" --target /home/u/下载/a.png --action write
  xl-taskd request task_01J... /home/u/下载 read write
  xl-taskd approve deleg_01J..."#
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
        return Err("用法：xl-taskd step <任务编号> <意图...> [--target <路径>] [--action <动作>]".into());
    }
    let task_id = args[0].clone();

    // 解析：intent 拼起来，--target / --action 单独取
    let mut intent_parts: Vec<String> = Vec::new();
    let mut target: Option<String> = None;
    let mut action_str: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                i += 1;
                target = args.get(i).cloned();
            }
            "--action" => {
                i += 1;
                action_str = args.get(i).cloned();
            }
            other => intent_parts.push(other.to_string()),
        }
        i += 1;
    }
    let intent = intent_parts.join(" ");
    if intent.is_empty() {
        return Err("缺少意图（intent 必填，保证系统能回答「为什么」）".into());
    }

    let store = open_store()?;
    let mut task = store
        .get_task(&task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("任务不存在：{task_id}"))?;

    // 进入运行态，这样越界暂停有合法转换
    if task.state == TaskState::Pending {
        task.transition_to(TaskState::Running)
            .map_err(|e| e.to_string())?;
        store
            .update_task_state(&task.id, TaskState::Running)
            .map_err(|e| e.to_string())?;
    }

    // ---- 委托检查 ----
    let mut used: Option<String> = None;
    if let (Some(t), Some(a)) = (&target, &action_str) {
        let action = parse_action(a)?;
        let delegations = store.list_delegations(&task.id).map_err(|e| e.to_string())?;
        match check_delegations(&delegations, t, action, now_ms()) {
            CheckOutcome::Allowed { delegation } => {
                store
                    .log_access(&task.id, Some(&delegation), t, action, true)
                    .map_err(|e| e.to_string())?;
                store.touch_delegation(&delegation).map_err(|e| e.to_string())?;
                used = Some(delegation);
            }
            CheckOutcome::NeedsPermission => {
                store
                    .log_access(&task.id, None, t, action, false)
                    .map_err(|e| e.to_string())?;
                if task.state == TaskState::Running {
                    let _ = task.transition_to(TaskState::Paused);
                    let _ = store.update_task_state(&task.id, TaskState::Paused);
                }
                return Err(format!(
                    "需要授权：{} 上的 {} 操作未被任何有效委托覆盖\n\
                     任务已暂停。申请方式：\n  xl-taskd request {} {} {}\n\
                     然后用 xl-taskd approve <委托编号> 批准。",
                    t, a, task.id, t, a
                ));
            }
        }
    }

    // 暂停中的任务在通过检查后恢复
    if task.state == TaskState::Paused {
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
    if let Some(d) = used {
        println!("  授权  {}（已计入审计）", d);
    }
    Ok(())
}

fn parse_action(s: &str) -> Result<Action, String> {
    match s {
        "read" => Ok(Action::Read),
        "write" => Ok(Action::Write),
        "delete" => Ok(Action::Delete),
        "execute" => Ok(Action::Execute),
        "net" => Ok(Action::Net),
        "device" => Ok(Action::Device),
        other => Err(format!("未知动作：{other}（可用 read/write/delete/execute/net/device）")),
    }
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

// ---------------------------------------------------------------- 委托

fn cmd_request(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("用法：xl-taskd request <任务编号> <范围> <动作...>".into());
    }
    let task_id = &args[0];
    let scope = vec![args[1].clone()];
    let actions: Vec<Action> = args[2..]
        .iter()
        .map(|a| parse_action(a))
        .collect::<Result<_, _>>()?;

    let store = open_store()?;
    store
        .get_task(task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("任务不存在：{task_id}"))?;

    let mut d = Delegation::new(
        task_id.clone(),
        Principal::User,
        scope,
        actions,
        TrustLevel::L1Restricted,
    );
    d.expires_at = Some(now_ms() + 600_000); // 默认 10 分钟
    store.save_delegation(&d).map_err(|e| e.to_string())?;

    println!("委托申请已创建（待批准）");
    println!("  编号  {}", d.id);
    println!("  范围  {:?}", d.scope);
    println!("  动作  {:?}", d.actions);
    println!("  批准  xl-taskd approve {}", d.id);
    Ok(())
}

fn cmd_approve(args: &[String]) -> Result<(), String> {
    let id = args.first().ok_or("用法：xl-taskd approve <委托编号>")?;
    let store = open_store()?;
    store.grant_delegation(id).map_err(|e| e.to_string())?;
    println!("委托已批准：{id}");
    println!("agent 可以重试之前被拒绝的步骤了。");
    Ok(())
}

fn cmd_delegations(args: &[String]) -> Result<(), String> {
    let task_id = args
        .first()
        .ok_or("用法：xl-taskd delegations <任务编号>")?;
    let store = open_store()?;
    let ds = store.list_delegations(task_id).map_err(|e| e.to_string())?;
    if ds.is_empty() {
        println!("该任务没有委托。");
        return Ok(());
    }
    let now = now_ms();
    println!("{:<32} {:<8} {:<8} {:<10} 范围 / 动作", "编号", "批准", "生效", "已用次数");
    for d in &ds {
        let granted = if d.granted { "是" } else { "否" };
        let active = if d.is_active(now) { "是" } else { "否" };
        println!(
            "{:<32} {:<8} {:<8} {:<10} {:?} {:?}",
            d.id,
            granted,
            active,
            format!(
                "{}/{}",
                d.ops_used,
                d.max_ops.map(|m| m.to_string()).unwrap_or_else(|| "∞".into())
            ),
            d.scope,
            d.actions
        );
    }
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
