//! MCP server：把 taskd 的四个原语暴露给任意 agent。
//!
//! 传输：stdio + JSON-RPC 2.0，每行一个 JSON 消息（stdout 只写协议，日志走 stderr）。
//! 协议：MCP。实现 `initialize` / `tools/list` / `tools/call` / `ping`。
//!
//! 设计约束（S0-DESIGN 5.2）：**工具数量要少**，每加一个都是长期负担。

use serde_json::{json, Value};
use taskd_core::*;
use taskd_store::Store;
use taskd_undo::{undo_to_unit, SnapshotStore};

pub const SERVER_NAME: &str = "xl-taskd";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// MCP 协议版本（回显客户端版本以最大化兼容）。
const DEFAULT_PROTOCOL: &str = "2024-11-05";

pub struct McpServer {
    store: Store,
    snapshots: SnapshotStore,
}

impl McpServer {
    pub fn new(store: Store, snapshots: SnapshotStore) -> Self {
        Self { store, snapshots }
    }

    /// 处理一行输入。返回 `None` 表示这是 notification（不需要回复）。
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(error_response(
                    Value::Null,
                    -32700,
                    &format!("解析失败：{e}"),
                ))
            }
        };

        let id = msg.get("id").cloned();
        let method = match msg.get("method").and_then(|m| m.as_str()) {
            Some(m) => m.to_string(),
            None => {
                // 没有 method：可能是 response（我们不发起请求，忽略）
                return None;
            }
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // notification 没有 id：处理但不回复
        let is_notification = id.is_none();

        let result = match method.as_str() {
            "initialize" => Ok(self.initialize(&params)),
            "notifications/initialized" | "notifications/cancelled" => return None,
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(&params),
            other => Err((-32601, format!("未知方法：{other}"))),
        };

        if is_notification {
            return None;
        }

        let id = id.unwrap_or(Value::Null);
        match result {
            Ok(v) => Some(success_response(id, v)),
            Err((code, msg)) => Some(error_response(id, code, &msg)),
        }
    }

    // ------------------------------------------------------------ initialize

    fn initialize(&self, params: &Value) -> Value {
        let protocol = params
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_PROTOCOL);

        json!({
            "protocolVersion": protocol,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            }
        })
    }

    // ------------------------------------------------------------ tools/list

    fn tools_list(&self) -> Value {
        json!({
            "tools": [
                {
                    "name": "task_begin",
                    "description": "创建一个任务。任务是有目标、有历史、可回滚的责任单位。返回任务编号。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "goal": { "type": "string", "description": "用户给出的目标，原文记录，不要改写" }
                        },
                        "required": ["goal"]
                    }
                },
                {
                    "name": "task_step",
                    "description": "为任务记录一步操作。intent 必填。若提供 target + action，会先做委托检查：未授权则返回 NEEDS_PERMISSION 并暂停任务。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" },
                            "intent": { "type": "string", "description": "这一步打算干什么（自然语言）" },
                            "kind": {
                                "type": "string",
                                "enum": ["tool_call", "decision", "user_input", "sub_task", "external"],
                                "description": "步骤类型，默认 tool_call"
                            },
                            "unit_id": { "type": "string", "description": "所属工作单元（可选）" },
                            "target": { "type": "string", "description": "操作对象（路径/URL/设备ID）。提供后触发委托检查" },
                            "action": {
                                "type": "string",
                                "enum": ["read", "write", "delete", "execute", "net", "device"],
                                "description": "操作类型，与 target 一起触发委托检查"
                            },
                            "taint": {
                                "type": "array",
                                "description": "本步接触的数据来源，会并入任务累计来源",
                                "items": {
                                    "type": "object",
                                    "description": "例：{\"kind\":\"external\",\"external_kind\":\"web\",\"origin\":\"https://example.com\"} 或 {\"kind\":\"local_untrusted\",\"path\":\"/下载/a.txt\"}"
                                }
                            }
                        },
                        "required": ["task_id", "intent"]
                    }
                },
                {
                    "name": "task_status",
                    "description": "查询任务的目标、状态、工作单元与全部步骤历史。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" }
                        },
                        "required": ["task_id"]
                    }
                },
                {
                    "name": "task_list",
                    "description": "列出所有任务（最近创建的在前）。",
                    "inputSchema": { "type": "object", "properties": {} }
                },
                {
                    "name": "task_done",
                    "description": "把任务标记为已完成。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" }
                        },
                        "required": ["task_id"]
                    }
                },
                {
                    "name": "unit_begin",
                    "description": "开始一个工作单元。撤销的粒度单位：attempt=一次试错，change=一个变更，release=一个可交付版本。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" },
                            "kind": {
                                "type": "string",
                                "enum": ["attempt", "change", "release"],
                                "description": "默认 attempt"
                            }
                        },
                        "required": ["task_id"]
                    }
                },
                {
                    "name": "unit_end",
                    "description": "结束一个工作单元并记录结果。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "unit_id": { "type": "string" },
                            "outcome": {
                                "type": "string",
                                "enum": ["succeeded", "failed", "abandoned"],
                                "description": "默认 succeeded"
                            }
                        },
                        "required": ["unit_id"]
                    }
                },
                {
                    "name": "task_undo",
                    "description": "撤销到某个工作单元之前（撤销该单元及其之后的所有单元）。文件通过快照恢复；不可撤销的效果会列在 skipped 里。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" },
                            "to_unit": { "type": "string", "description": "目标工作单元编号；省略则撤销全部" }
                        },
                        "required": ["task_id"]
                    }
                },
                {
                    "name": "taint_report",
                    "description": "查看任务累计的数据来源、不可信来源清单、以及是否已被确认。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" }
                        },
                        "required": ["task_id"]
                    }
                },
                {
                    "name": "taint_acknowledge",
                    "description": "确认「已让用户看过这些不可信来源」。确认后来源集合若变化会自动失效。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" },
                            "reason": { "type": "string", "description": "用户确认的说明（可选）" }
                        },
                        "required": ["task_id"]
                    }
                },
                {
                    "name": "delegation_request",
                    "description": "申请权限委托。创建的是未批准的请求，必须由人类批准后才生效（agent 不能自己批准）。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" },
                            "scope": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "资源范围：路径 / URL 域名 / 设备ID 的 glob"
                            },
                            "actions": {
                                "type": "array",
                                "items": {
                                    "type": "string",
                                    "enum": ["read", "write", "delete", "execute", "net", "device"]
                                }
                            },
                            "ttl_secs": { "type": "integer", "description": "有效期（秒），默认 600" },
                            "max_ops": { "type": "integer", "description": "最大操作次数（可选）" }
                        },
                        "required": ["task_id", "scope", "actions"]
                    }
                },
                {
                    "name": "delegation_list",
                    "description": "查看任务的全部委托及其批准/生效状态。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" }
                        },
                        "required": ["task_id"]
                    }
                }
            ]
        })
    }

    // ------------------------------------------------------------ tools/call

    fn tools_call(&mut self, params: &Value) -> Result<Value, (i32, String)> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "缺少工具名".to_string()))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));

        let result = match name {
            "task_begin" => self.tool_task_begin(&args),
            "task_step" => self.tool_task_step(&args),
            "task_status" => self.tool_task_status(&args),
            "task_list" => self.tool_task_list(),
            "task_done" => self.tool_task_done(&args),
            "unit_begin" => self.tool_unit_begin(&args),
            "unit_end" => self.tool_unit_end(&args),
            "task_undo" => self.tool_task_undo(&args),
            "taint_report" => self.tool_taint_report(&args),
            "taint_acknowledge" => self.tool_taint_acknowledge(&args),
            "delegation_request" => self.tool_delegation_request(&args),
            "delegation_list" => self.tool_delegation_list(&args),
            other => return Err((-32602, format!("未知工具：{other}"))),
        };

        match result {
            Ok(v) => Ok(json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
                "isError": false
            })),
            Err(e) => Ok(json!({
                "content": [{ "type": "text", "text": format!("错误：{e}") }],
                "isError": true
            })),
        }
    }

    // ------------------------------------------------------------ 工具实现

    fn tool_task_begin(&self, args: &Value) -> Result<Value, String> {
        let goal = str_arg(args, "goal")?;
        let task = Task::new(goal);
        self.store.create_task(&task).map_err(|e| e.to_string())?;
        Ok(json!({
            "task_id": task.id,
            "goal": task.goal,
            "state": state_label(task.state)
        }))
    }

    fn tool_task_step(&self, args: &Value) -> Result<Value, String> {
        let task_id = str_arg(args, "task_id")?;
        let intent = str_arg(args, "intent")?;
        let taint = parse_taint_arg(args)?;

        let mut task = self
            .store
            .get_task(&task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;

        // 任何一步操作都意味着任务已开始，先把 Pending 变成 Running，
        // 这样后续的「越界暂停」才有合法状态转换可用。
        if task.state == TaskState::Pending {
            task.transition_to(TaskState::Running)
                .map_err(|e| e.to_string())?;
            self.store
                .update_task_state(&task.id, TaskState::Running)
                .map_err(|e| e.to_string())?;
        }

        // ---- 委托检查（S0.2）+ 自动快照（S0.3）----
        let target = args.get("target").and_then(|v| v.as_str());
        let action_str = args.get("action").and_then(|v| v.as_str());
        let mut used_delegation: Option<String> = None;
        let mut effect: Option<Effect> = None;
        let mut snapshot_id: Option<String> = None;

        if let (Some(t), Some(a)) = (target, action_str) {
            let action = parse_action(a)?;
            let delegations = self
                .store
                .list_delegations(&task.id)
                .map_err(|e| e.to_string())?;

            match check_delegations(&delegations, t, action, now_ms()) {
                CheckOutcome::Allowed { delegation } => {
                    self.store
                        .log_access(&task.id, Some(&delegation), t, action, true)
                        .map_err(|e| e.to_string())?;
                    self.store
                        .touch_delegation(&delegation)
                        .map_err(|e| e.to_string())?;
                    used_delegation = Some(delegation);
                }
                CheckOutcome::NeedsPermission => {
                    self.store
                        .log_access(&task.id, None, t, action, false)
                        .map_err(|e| e.to_string())?;
                    // P-4：越界 = 暂停 + 请求，不是拒绝 + 继续
                    if task.state == TaskState::Running {
                        let _ = task.transition_to(TaskState::Paused);
                        let _ = self.store.update_task_state(&task.id, TaskState::Paused);
                    }
                    return Err(serde_json::to_string(&json!({
                        "error": "NEEDS_PERMISSION",
                        "task_id": task.id,
                        "target": t,
                        "action": a,
                        "task_state": "paused",
                        "hint": "调用 delegation_request 申请委托，等人类批准后重试本步骤"
                    }))
                    .unwrap_or_default());
                }
            }

            // 写/删之前先留底，否则这一步不可撤销
            if matches!(action, Action::Write | Action::Delete) && looks_like_path(t) {
                // ---- 溯源检查（S0.4）：任务接触过不可信来源时，先让人确认 ----
                if let Err(needs_confirm) = self.taint_gate(&task.id, &taint) {
                    return Err(needs_confirm);
                }

                let path = std::path::PathBuf::from(t);
                match self.snapshots.capture(&path) {
                    Ok(meta) => {
                        snapshot_id = Some(meta.id.clone());
                        effect = Some(Effect::SnapshotRef {
                            root: path.display().to_string(),
                            snapshot: meta.id,
                        });
                    }
                    Err(e) => {
                        return Err(format!(
                            "无法对 {} 做快照：{e}\n该操作将不可撤销，已拒绝执行。",
                            path.display()
                        ));
                    }
                }
            }
        }

        // 暂停中的任务在通过检查后恢复运行
        if task.state == TaskState::Paused {
            task.transition_to(TaskState::Running)
                .map_err(|e| e.to_string())?;
            self.store
                .update_task_state(&task.id, TaskState::Running)
                .map_err(|e| e.to_string())?;
        } else if task.state == TaskState::Pending {
            task.transition_to(TaskState::Running)
                .map_err(|e| e.to_string())?;
            self.store
                .update_task_state(&task.id, TaskState::Running)
                .map_err(|e| e.to_string())?;
        }

        let idx = self
            .store
            .next_step_index(&task.id)
            .map_err(|e| e.to_string())?;

        let kind = match args.get("kind").and_then(|v| v.as_str()) {
            Some("decision") => StepKind::Decision,
            Some("user_input") => StepKind::UserInput,
            Some("sub_task") => StepKind::SubTask,
            Some("external") => StepKind::External,
            _ => StepKind::ToolCall,
        };

        let mut step = Step::new(&task.id, idx, kind, &intent);
        if let Some(unit) = args.get("unit_id").and_then(|v| v.as_str()) {
            step = step.with_unit(unit);
        }
        if let Some(e) = effect {
            step = step.with_effect(e);
        }
        step = step.with_taint(taint);
        step.finish(StepResult::Ok { payload: None });
        self.store
            .append_step(&step)
            .map_err(|e| e.to_string())?;

        Ok(json!({
            "task_id": task.id,
            "index": idx,
            "intent": step.intent,
            "risk": risk_label(step.risk),
            "delegation": used_delegation,
            "snapshot": snapshot_id
        }))
    }

    // ------------------------------------------------------------ 溯源

    /// 写操作前的溯源闸门：任务接触过不可信来源、且用户尚未确认时拦截。
    fn taint_gate(&self, task_id: &str, extra: &TaintSet) -> Result<(), String> {
        let mut combined = self
            .store
            .task_taint(task_id)
            .map_err(|e| e.to_string())?;
        combined.merge(extra);

        if !combined.has_untrusted() {
            return Ok(());
        }

        let fp = combined.fingerprint();
        let acked = self
            .store
            .get_taint_ack(task_id)
            .map_err(|e| e.to_string())?;
        if matches!(&acked, Some((h, _, _)) if h == &fp) {
            return Ok(());
        }

        Err(serde_json::to_string(&json!({
            "error": "NEEDS_CONFIRMATION",
            "task_id": task_id,
            "untrusted_sources": combined.untrusted_descriptions(),
            "hint": "把这些来源展示给用户并请求确认；确认后调用 taint_acknowledge 再重试本步骤"
        }))
        .unwrap_or_default())
    }

    fn tool_taint_report(&self, args: &Value) -> Result<Value, String> {
        let task_id = str_arg(args, "task_id")?;
        let taint = self
            .store
            .task_taint(&task_id)
            .map_err(|e| e.to_string())?;
        let acked = self
            .store
            .get_taint_ack(&task_id)
            .map_err(|e| e.to_string())?;

        Ok(json!({
            "task_id": task_id,
            "sources": taint.sources().iter().map(|s| s.describe()).collect::<Vec<_>>(),
            "untrusted": taint.untrusted_descriptions(),
            "has_untrusted": taint.has_untrusted(),
            "fingerprint": taint.fingerprint(),
            "acknowledged": matches!(&acked, Some((h, _, _)) if h == &taint.fingerprint())
        }))
    }

    fn tool_taint_acknowledge(&self, args: &Value) -> Result<Value, String> {
        let task_id = str_arg(args, "task_id")?;
        let reason = args.get("reason").and_then(|v| v.as_str());

        let taint = self
            .store
            .task_taint(&task_id)
            .map_err(|e| e.to_string())?;
        let fp = taint.fingerprint();
        self.store
            .ack_taint(&task_id, &fp, reason)
            .map_err(|e| e.to_string())?;

        Ok(json!({
            "task_id": task_id,
            "acknowledged_fingerprint": fp,
            "untrusted_sources": taint.untrusted_descriptions(),
            "note": "若之后出现新的不可信来源，确认会自动失效"
        }))
    }

    // ------------------------------------------------------------ 撤销

    fn tool_task_undo(&self, args: &Value) -> Result<Value, String> {
        let task_id = str_arg(args, "task_id")?;
        let to_unit = args.get("to_unit").and_then(|v| v.as_str());

        let task = self
            .store
            .get_task(&task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;

        let report = undo_to_unit(&self.store, &self.snapshots, &task.id, to_unit)
            .map_err(|e| format!("撤销失败：{e}"))?;

        Ok(json!({
            "task_id": task.id,
            "units_undone": report.units_undone,
            "steps_reverted": report.steps_reverted,
            "files_restored": report.files_restored,
            "skipped": report.skipped,
            "elapsed_ms": report.elapsed_ms
        }))
    }

    // ------------------------------------------------------------ 委托

    fn tool_delegation_request(&self, args: &Value) -> Result<Value, String> {
        let task_id = str_arg(args, "task_id")?;
        let scope = str_array_arg(args, "scope")?;
        let actions_raw = str_array_arg(args, "actions")?;
        let actions: Vec<Action> = actions_raw
            .iter()
            .map(|a| parse_action(a))
            .collect::<Result<_, _>>()?;

        let ttl_secs = args
            .get("ttl_secs")
            .and_then(|v| v.as_i64())
            .unwrap_or(600);
        let max_ops = args.get("max_ops").and_then(|v| v.as_u64());

        // 确认任务存在
        self.store
            .get_task(&task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;

        let mut d = Delegation::new(
            task_id.clone(),
            Principal::User,
            scope.clone(),
            actions.clone(),
            TrustLevel::L1Restricted,
        );
        d.expires_at = Some(now_ms() + ttl_secs * 1000);
        d.max_ops = max_ops;
        // granted 保持 false：批准是人的动作，agent 不能自己批准
        self.store.save_delegation(&d).map_err(|e| e.to_string())?;

        Ok(json!({
            "delegation_id": d.id,
            "task_id": task_id,
            "scope": scope,
            "actions": actions,
            "status": "pending_approval",
            "message": format!("等待人类批准：xl-taskd approve {}", d.id)
        }))
    }

    fn tool_delegation_list(&self, args: &Value) -> Result<Value, String> {
        let task_id = str_arg(args, "task_id")?;
        let delegations = self
            .store
            .list_delegations(&task_id)
            .map_err(|e| e.to_string())?;
        let now = now_ms();
        let items: Vec<Value> = delegations
            .iter()
            .map(|d| {
                json!({
                    "delegation_id": d.id,
                    "scope": d.scope,
                    "actions": d.actions,
                    "granted": d.granted,
                    "active": d.is_active(now),
                    "expires_at": d.expires_at,
                    "ops_used": d.ops_used,
                    "max_ops": d.max_ops
                })
            })
            .collect();
        Ok(json!({ "delegations": items }))
    }

    fn tool_task_status(&self, args: &Value) -> Result<Value, String> {
        let task_id = str_arg(args, "task_id")?;
        let task = self
            .store
            .get_task(&task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;
        let steps = self.store.list_steps(&task.id).map_err(|e| e.to_string())?;
        let units = self.store.list_units(&task.id).map_err(|e| e.to_string())?;

        let steps_json: Vec<Value> = steps
            .iter()
            .map(|s| {
                json!({
                    "index": s.index,
                    "intent": s.intent,
                    "risk": risk_label(s.risk),
                    "unit_id": s.unit_id,
                })
            })
            .collect();

        let units_json: Vec<Value> = units
            .iter()
            .map(|u| {
                json!({
                    "unit_id": u.id,
                    "kind": unit_kind_label(u.kind),
                    "outcome": outcome_label(u.outcome),
                })
            })
            .collect();

        Ok(json!({
            "task_id": task.id,
            "goal": task.goal,
            "state": state_label(task.state),
            "created_at": task.created_at,
            "steps": steps_json,
            "work_units": units_json
        }))
    }

    fn tool_task_list(&self) -> Result<Value, String> {
        let tasks = self.store.list_tasks().map_err(|e| e.to_string())?;
        let items: Vec<Value> = tasks
            .iter()
            .map(|t| {
                json!({
                    "task_id": t.id,
                    "goal": t.goal,
                    "state": state_label(t.state)
                })
            })
            .collect();
        Ok(json!({ "tasks": items }))
    }

    fn tool_task_done(&self, args: &Value) -> Result<Value, String> {
        let task_id = str_arg(args, "task_id")?;
        let mut task = self
            .store
            .get_task(&task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;
        task.transition_to(TaskState::Completed)
            .map_err(|e| e.to_string())?;
        self.store
            .update_task_state(&task.id, TaskState::Completed)
            .map_err(|e| e.to_string())?;
        Ok(json!({ "task_id": task.id, "state": state_label(task.state) }))
    }

    fn tool_unit_begin(&self, args: &Value) -> Result<Value, String> {
        let task_id = str_arg(args, "task_id")?;
        let kind = match args.get("kind").and_then(|v| v.as_str()) {
            Some("change") => WorkUnitKind::Change,
            Some("release") => WorkUnitKind::Release,
            _ => WorkUnitKind::Attempt,
        };
        let task = self
            .store
            .get_task(&task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;
        let unit = WorkUnit::new(&task.id, kind);
        self.store.create_unit(&unit).map_err(|e| e.to_string())?;
        Ok(json!({
            "unit_id": unit.id,
            "kind": unit_kind_label(kind)
        }))
    }

    fn tool_unit_end(&self, args: &Value) -> Result<Value, String> {
        let unit_id = str_arg(args, "unit_id")?;
        let outcome = match args.get("outcome").and_then(|v| v.as_str()) {
            Some("failed") => Outcome::Failed,
            Some("abandoned") => Outcome::Abandoned,
            _ => Outcome::Succeeded,
        };
        self.store
            .finish_unit(&unit_id, outcome)
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "unit_id": unit_id,
            "outcome": outcome_label(outcome)
        }))
    }
}

// ---------------------------------------------------------------- helpers

fn str_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("缺少参数：{key}"))
}

fn str_array_arg(args: &Value, key: &str) -> Result<Vec<String>, String> {
    let arr = args
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("缺少参数：{key}（应为字符串数组）"))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| format!("参数 {key} 里含非字符串元素"))
        })
        .collect()
}

fn parse_action(s: &str) -> Result<Action, String> {
    match s {
        "read" => Ok(Action::Read),
        "write" => Ok(Action::Write),
        "delete" => Ok(Action::Delete),
        "execute" => Ok(Action::Execute),
        "net" => Ok(Action::Net),
        "device" => Ok(Action::Device),
        other => Err(format!("未知动作：{other}")),
    }
}

/// 粗略判断 target 是否是文件系统路径（而不是 URL / 设备名 / 逻辑名）。
fn looks_like_path(s: &str) -> bool {
    if s.starts_with("http://") || s.starts_with("https://") {
        return false;
    }
    // Unix 绝对/相对路径
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") {
        return true;
    }
    // Windows 盘符：C:\ 或 C:/
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// 解析 `taint` 参数为 TaintSet。
fn parse_taint_arg(args: &Value) -> Result<TaintSet, String> {
    let mut set = TaintSet::default();
    if let Some(arr) = args.get("taint").and_then(|v| v.as_array()) {
        for item in arr {
            let src: TaintSource = serde_json::from_value(item.clone())
                .map_err(|e| format!("taint 项格式错误：{e}"))?;
            set.add(src);
        }
    }
    Ok(set)
}

fn success_response(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: Value, code: i32, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
    .to_string()
}

fn state_label(s: TaskState) -> &'static str {
    match s {
        TaskState::Pending => "pending",
        TaskState::Running => "running",
        TaskState::Paused => "paused",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::RolledBack => "rolled_back",
        TaskState::Deployed => "deployed",
    }
}

fn unit_kind_label(k: WorkUnitKind) -> &'static str {
    match k {
        WorkUnitKind::Attempt => "attempt",
        WorkUnitKind::Change => "change",
        WorkUnitKind::Release => "release",
    }
}

fn outcome_label(o: Outcome) -> &'static str {
    match o {
        Outcome::Running => "running",
        Outcome::Succeeded => "succeeded",
        Outcome::Failed => "failed",
        Outcome::Abandoned => "abandoned",
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

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> McpServer {
        let dir = std::env::temp_dir().join(format!(
            "taskd-mcp-test-{}-{}",
            std::process::id(),
            taskd_core::new_id("t")
        ));
        let snaps = SnapshotStore::new(dir).expect("snapshot store");
        McpServer::new(Store::open_in_memory().unwrap(), snaps)
    }

    fn call(s: &mut McpServer, line: &str) -> Value {
        let out = s.handle_line(line).expect("expected a response");
        serde_json::from_str(&out).expect("valid json")
    }

    #[test]
    fn initialize_echoes_protocol() {
        let mut s = server();
        let v = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        );
        assert_eq!(v["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(v["result"]["serverInfo"]["name"], "xl-taskd");
    }

    #[test]
    fn notification_gets_no_response() {
        let mut s = server();
        assert!(s
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }

    #[test]
    fn tools_list_has_expected_tools() {
        let mut s = server();
        let v = call(&mut s, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let names: Vec<&str> = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "task_begin",
            "task_step",
            "task_status",
            "task_list",
            "task_done",
            "unit_begin",
            "unit_end",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
    }

    #[test]
    fn full_task_lifecycle_over_mcp() {
        let mut s = server();

        // begin
        let v = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"task_begin","arguments":{"goal":"整理下载文件夹"}}}"#,
        );
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        let task_id = parsed["task_id"].as_str().unwrap().to_string();
        assert!(task_id.starts_with("task_"));

        // steps
        for intent in ["列出文件", "创建目录", "移动文件"] {
            let req = format!(
                r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"task_step","arguments":{{"task_id":"{task_id}","intent":"{intent}"}}}}}}"#
            );
            let v = call(&mut s, &req);
            assert_eq!(v["result"]["isError"], false);
        }

        // status
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{{"name":"task_status","arguments":{{"task_id":"{task_id}"}}}}}}"#
        );
        let v = call(&mut s, &req);
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["state"], "running");
        assert_eq!(parsed["steps"].as_array().unwrap().len(), 3);
    }

    // ---------------------------------------------------------- S0.2 委托

    fn begin_task(s: &mut McpServer, goal: &str) -> String {
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{{"name":"task_begin","arguments":{{"goal":"{goal}"}}}}}}"#
        );
        let v = call(s, &req);
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        parsed["task_id"].as_str().unwrap().to_string()
    }

    fn tool_text(v: &Value) -> String {
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn step_req(task_id: &str, intent: &str, target: &str, action: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{{"name":"task_step","arguments":{{"task_id":"{task_id}","intent":"{intent}","target":"{target}","action":"{action}"}}}}}}"#
        )
    }

    #[test]
    fn step_without_delegation_needs_permission_and_pauses() {
        let mut s = server();
        let task_id = begin_task(&mut s, "整理下载目录");

        let v = call(
            &mut s,
            &step_req(&task_id, "移动文件", "/home/u/下载/a.png", "write"),
        );
        assert_eq!(v["result"]["isError"], true);
        let text = tool_text(&v);
        assert!(text.contains("NEEDS_PERMISSION"), "got: {text}");

        // P-4：越界后任务应被暂停
        let t = s.store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(t.state, TaskState::Paused);
    }

    #[test]
    fn delegation_requires_human_approval() {
        let mut s = server();
        let task_id = begin_task(&mut s, "整理下载目录");

        // agent 申请委托
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{{"name":"delegation_request","arguments":{{"task_id":"{task_id}","scope":["/home/u/下载"],"actions":["read","write"],"ttl_secs":600}}}}}}"#
        );
        let v = call(&mut s, &req);
        let text = tool_text(&v);
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let deleg_id = parsed["delegation_id"].as_str().unwrap().to_string();
        assert_eq!(parsed["status"], "pending_approval");

        // 未批准 → 仍然拒绝
        let v = call(
            &mut s,
            &step_req(&task_id, "移动文件", "/home/u/下载/a.png", "write"),
        );
        assert_eq!(v["result"]["isError"], true);

        // 人类批准（直接操作 store，等价于 CLI approve）
        s.store.grant_delegation(&deleg_id).unwrap();

        // 重试 → 通过，且任务恢复运行
        let v = call(
            &mut s,
            &step_req(&task_id, "移动文件", "/home/u/下载/a.png", "write"),
        );
        assert_eq!(v["result"]["isError"], false);
        let t = s.store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(t.state, TaskState::Running);
    }

    #[test]
    fn out_of_scope_still_denied_after_grant() {
        let mut s = server();
        let task_id = begin_task(&mut s, "整理下载目录");

        let req = format!(
            r#"{{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{{"name":"delegation_request","arguments":{{"task_id":"{task_id}","scope":["/home/u/下载"],"actions":["read"]}}}}}}"#
        );
        let v = call(&mut s, &req);
        let parsed: Value = serde_json::from_str(&tool_text(&v)).unwrap();
        let deleg_id = parsed["delegation_id"].as_str().unwrap().to_string();
        s.store.grant_delegation(&deleg_id).unwrap();

        // 范围外
        let v = call(
            &mut s,
            &step_req(&task_id, "读密码", "/home/u/密码.txt", "read"),
        );
        assert_eq!(v["result"]["isError"], true);

        // 动作外（read 委托做不了 write）
        let v = call(
            &mut s,
            &step_req(&task_id, "写文件", "/home/u/下载/a.png", "write"),
        );
        assert_eq!(v["result"]["isError"], true);

        // 范围内 read 通过
        let v = call(
            &mut s,
            &step_req(&task_id, "读文件", "/home/u/下载/a.png", "read"),
        );
        assert_eq!(v["result"]["isError"], false);
    }

    #[test]
    fn access_log_records_both_outcomes() {
        let mut s = server();
        let task_id = begin_task(&mut s, "整理下载目录");

        // 一次拒绝
        let _ = call(
            &mut s,
            &step_req(&task_id, "移动文件", "/home/u/下载/a.png", "write"),
        );

        let denied: i64 = s
            .store
            .audit_count(&task_id, false)
            .expect("query access log");
        assert_eq!(denied, 1, "拒绝应被记入审计");
    }

    // ---------------------------------------------------------- S0.4 溯源

    fn grant_delegation(s: &mut McpServer, task_id: &str, scope: &str, actions: &[&str]) {
        let actions_json = actions
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(",");
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{{"name":"delegation_request","arguments":{{"task_id":"{task_id}","scope":["{scope}"],"actions":[{actions_json}]}}}}}}"#
        );
        let v = call(s, &req);
        let parsed: Value = serde_json::from_str(&tool_text(&v)).unwrap();
        let id = parsed["delegation_id"].as_str().unwrap().to_string();
        s.store.grant_delegation(&id).unwrap();
    }

    #[test]
    fn untrusted_source_blocks_write_until_acknowledged() {
        let mut s = server();
        let task_id = begin_task(&mut s, "整理外部文档");

        // 记录一个外部来源（只记录，不写）
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{{"name":"task_step","arguments":{{"task_id":"{task_id}","intent":"读取网页内容","taint":[{{"kind":"external","external_kind":"web","origin":"https://evil.example"}}]}}}}}}"#
        );
        let v = call(&mut s, &req);
        assert_eq!(v["result"]["isError"], false);

        grant_delegation(&mut s, &task_id, "/tmp", &["write"]);

        // 写操作 → 被溯源闸门拦住
        let v = call(
            &mut s,
            &step_req(&task_id, "写入文件", "/tmp/x.txt", "write"),
        );
        assert_eq!(v["result"]["isError"], true);
        let text = tool_text(&v);
        assert!(text.contains("NEEDS_CONFIRMATION"), "got: {text}");
        assert!(text.contains("evil.example"), "应列出不可信来源，got: {text}");

        // 确认
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{{"name":"taint_acknowledge","arguments":{{"task_id":"{task_id}","reason":"用户已阅读来源"}}}}}}"#
        );
        let v = call(&mut s, &req);
        assert_eq!(v["result"]["isError"], false);

        // 重试 → 通过
        let v = call(
            &mut s,
            &step_req(&task_id, "写入文件", "/tmp/x.txt", "write"),
        );
        assert_eq!(v["result"]["isError"], false);
    }

    #[test]
    fn ack_invalidated_by_new_untrusted_source() {
        let mut s = server();
        let task_id = begin_task(&mut s, "多来源任务");

        let req = format!(
            r#"{{"jsonrpc":"2.0","id":23,"method":"tools/call","params":{{"name":"task_step","arguments":{{"task_id":"{task_id}","intent":"读网页","taint":[{{"kind":"external","external_kind":"web","origin":"https://a.example"}}]}}}}}}"#
        );
        call(&mut s, &req);
        grant_delegation(&mut s, &task_id, "/tmp", &["write"]);

        // 确认当前来源集合
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":24,"method":"tools/call","params":{{"name":"taint_acknowledge","arguments":{{"task_id":"{task_id}"}}}}}}"#
        );
        call(&mut s, &req);

        // 第一次写通过
        let v = call(&mut s, &step_req(&task_id, "写1", "/tmp/a.txt", "write"));
        assert_eq!(v["result"]["isError"], false);

        // 出现新的不可信来源
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":25,"method":"tools/call","params":{{"name":"task_step","arguments":{{"task_id":"{task_id}","intent":"又读了一个网页","taint":[{{"kind":"external","external_kind":"web","origin":"https://b.example"}}]}}}}}}"#
        );
        let v = call(&mut s, &req);
        assert_eq!(v["result"]["isError"], false);

        // 之前的确认应失效 → 写被拦
        let v = call(&mut s, &step_req(&task_id, "写2", "/tmp/b.txt", "write"));
        assert_eq!(v["result"]["isError"], true);
        assert!(tool_text(&v).contains("NEEDS_CONFIRMATION"));
    }

    #[test]
    fn taint_report_lists_sources() {
        let mut s = server();
        let task_id = begin_task(&mut s, "来源报告");

        let req = format!(
            r#"{{"jsonrpc":"2.0","id":26,"method":"tools/call","params":{{"name":"task_step","arguments":{{"task_id":"{task_id}","intent":"读本地文件","taint":[{{"kind":"local_untrusted","path":"/home/u/下载/a.txt"}}]}}}}}}"#
        );
        call(&mut s, &req);

        let req = format!(
            r#"{{"jsonrpc":"2.0","id":27,"method":"tools/call","params":{{"name":"taint_report","arguments":{{"task_id":"{task_id}"}}}}}}"#
        );
        let v = call(&mut s, &req);
        let parsed: Value = serde_json::from_str(&tool_text(&v)).unwrap();
        assert_eq!(parsed["has_untrusted"], true);
        assert_eq!(parsed["acknowledged"], false);
        assert!(parsed["untrusted"][0]
            .as_str()
            .unwrap()
            .contains("下载"));
    }

    #[test]
    fn trusted_sources_do_not_block_writes() {
        let mut s = server();
        let task_id = begin_task(&mut s, "只读可信文件");

        let req = format!(
            r#"{{"jsonrpc":"2.0","id":28,"method":"tools/call","params":{{"name":"task_step","arguments":{{"task_id":"{task_id}","intent":"读自己的配置","taint":[{{"kind":"local_trusted","path":"/home/u/.config/a"}}]}}}}}}"#
        );
        call(&mut s, &req);
        grant_delegation(&mut s, &task_id, "/tmp", &["write"]);

        // 可信来源不应触发确认
        let v = call(&mut s, &step_req(&task_id, "写", "/tmp/c.txt", "write"));
        assert_eq!(v["result"]["isError"], false);
    }

    #[test]
    fn unknown_tool_returns_tool_level_error() {
        let mut s = server();
        let v = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        );
        assert!(v["error"].is_object());
    }

    #[test]
    fn missing_argument_is_tool_error_not_crash() {
        let mut s = server();
        let v = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"task_begin","arguments":{}}}"#,
        );
        assert_eq!(v["result"]["isError"], true);
        assert!(v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("goal"));
    }

    #[test]
    fn unknown_method_returns_jsonrpc_error() {
        let mut s = server();
        let v = call(&mut s, r#"{"jsonrpc":"2.0","id":8,"method":"bogus"}"#);
        assert_eq!(v["error"]["code"], -32601);
    }

    #[test]
    fn bad_json_returns_parse_error() {
        let mut s = server();
        let v = call(&mut s, "{not json");
        assert_eq!(v["error"]["code"], -32700);
    }
}
