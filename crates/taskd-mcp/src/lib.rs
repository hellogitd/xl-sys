//! MCP server：把 taskd 的四个原语暴露给任意 agent。
//!
//! 传输：stdio + JSON-RPC 2.0，每行一个 JSON 消息（stdout 只写协议，日志走 stderr）。
//! 协议：MCP。实现 `initialize` / `tools/list` / `tools/call` / `ping`。
//!
//! 设计约束（S0-DESIGN 5.2）：**工具数量要少**，每加一个都是长期负担。

use serde_json::{json, Value};
use taskd_core::*;
use taskd_store::Store;

pub const SERVER_NAME: &str = "xl-taskd";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// MCP 协议版本（回显客户端版本以最大化兼容）。
const DEFAULT_PROTOCOL: &str = "2024-11-05";

pub struct McpServer {
    store: Store,
}

impl McpServer {
    pub fn new(store: Store) -> Self {
        Self { store }
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

        // ---- 委托检查（S0.2）：带 target + action 的步骤必须先获得授权 ----
        let mut used_delegation: Option<String> = None;
        if let (Some(target), Some(action_str)) = (
            args.get("target").and_then(|v| v.as_str()),
            args.get("action").and_then(|v| v.as_str()),
        ) {
            let action = parse_action(action_str)?;
            let delegations = self
                .store
                .list_delegations(&task.id)
                .map_err(|e| e.to_string())?;

            match check_delegations(&delegations, target, action, now_ms()) {
                CheckOutcome::Allowed { delegation } => {
                    self.store
                        .log_access(&task.id, Some(&delegation), target, action, true)
                        .map_err(|e| e.to_string())?;
                    self.store
                        .touch_delegation(&delegation)
                        .map_err(|e| e.to_string())?;
                    used_delegation = Some(delegation);
                }
                CheckOutcome::NeedsPermission => {
                    self.store
                        .log_access(&task.id, None, target, action, false)
                        .map_err(|e| e.to_string())?;
                    // P-4：越界 = 暂停 + 请求，不是拒绝 + 继续
                    if task.state == TaskState::Running {
                        let _ = task.transition_to(TaskState::Paused);
                        let _ = self.store.update_task_state(&task.id, TaskState::Paused);
                    }
                    return Err(serde_json::to_string(&json!({
                        "error": "NEEDS_PERMISSION",
                        "task_id": task.id,
                        "target": target,
                        "action": action_str,
                        "task_state": "paused",
                        "hint": "调用 delegation_request 申请委托，等人类批准后重试本步骤"
                    }))
                    .unwrap_or_default());
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
        step.finish(StepResult::Ok { payload: None });
        self.store
            .append_step(&step)
            .map_err(|e| e.to_string())?;

        Ok(json!({
            "task_id": task.id,
            "index": idx,
            "intent": step.intent,
            "risk": risk_label(step.risk),
            "delegation": used_delegation
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
        McpServer::new(Store::open_in_memory().unwrap())
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
