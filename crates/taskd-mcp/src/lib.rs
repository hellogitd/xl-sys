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
                    "description": "为任务记录一步操作。intent 必填，保证系统永远能回答「这一步想干什么」。",
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
                            "unit_id": { "type": "string", "description": "所属工作单元（可选）" }
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

        if task.state == TaskState::Pending {
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
            "risk": risk_label(step.risk)
        }))
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
