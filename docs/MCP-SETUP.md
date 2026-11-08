# 接入 xl-taskd（MCP）

`xl-taskd-mcp` 是一个标准 MCP server，任何支持 MCP 的 agent 都能接。

## 1. 构建

```bash
cargo build --release -p taskd-mcp
```

产物：`target/release/xl-taskd-mcp.exe`（Linux/macOS 无 `.exe`）

## 2. 配置

MCP 客户端配置里加一条：

```json
{
  "mcpServers": {
    "xl-taskd": {
      "command": "/绝对路径/target/release/xl-taskd-mcp.exe",
      "env": {
        "XLTASKD_DB": "/绝对路径/taskd.db"
      }
    }
  }
}
```

**要点：**

- `command` 必须用**绝对路径**
- `XLTASKD_DB` 建议显式指定，否则数据库落在进程当前目录
- 不需要任何网络端口，纯 stdio

### 常见客户端位置

| 客户端 | 配置文件 |
|---|---|
| Claude Desktop (Windows) | `%APPDATA%\Claude\claude_desktop_config.json` |
| Claude Desktop (macOS) | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Cursor | `~/.cursor/mcp.json` |
| DSH | 见 DSH 自己的 MCP 配置（`mcp_connector_configure` 或配置文件） |

## 3. 验证

在客户端里看到 `xl-taskd` 的 9 个工具即为接入成功：

| 工具 | 用途 |
|---|---|
| `task_begin` | 创建任务，返回 `task_id` |
| `task_step` | 记录一步（`intent` 必填）；带 `target`+`action` 时做委托检查 |
| `task_status` | 查历史 |
| `task_list` | 列任务 |
| `task_done` | 标记完成 |
| `unit_begin` | 开始工作单元（attempt/change/release） |
| `unit_end` | 结束工作单元 |
| `delegation_request` | 申请委托（**创建的是待批准请求**） |
| `delegation_list` | 查看委托及其批准/生效状态 |

### 委托流转（S0.2）

```
agent: task_step(target=/home/u/下载/a.png, action=write)
  ← 错误 NEEDS_PERMISSION，任务暂停

agent: delegation_request(scope=["/home/u/下载"], actions=["read","write"])
  ← { delegation_id: deleg_xxx, status: "pending_approval" }

人类:  xl-taskd approve deleg_xxx

agent: task_step(...) 重试
  ← 成功，任务恢复运行，审计已记录
```

**agent 不能自己批准委托**——批准是人的动作，通过 CLI 完成。这是 S0.2 的核心安全性质。

## 4. 手工测协议（不接客户端）

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"cli","version":"1"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"task_begin","arguments":{"goal":"test task"}}}' \
  | ./target/release/xl-taskd-mcp
```

Windows PowerShell：

```powershell
$lines = @(
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"cli","version":"1"}}}',
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"task_begin","arguments":{"goal":"test task"}}}'
)
$lines | & .\target\release\xl-taskd-mcp.exe
```

## 5. 已知限制（S0.1 阶段）

| 限制 | 说明 | 计划 |
|---|---|---|
| 无撤销 | 只能记录，不能回退 | S0.3 |
| 无溯源 | 不记录数据来源 | S0.4 |
| 无 effect 参数 | `task_step` 暂不接受副作用描述 | S0.3 |
| 检查可被绕过 | agent 若直接调自己的文件工具，不经过 task_step | S3（内核 capability） |

**委托检查只在「操作经过 task_step」时生效。** 真隔离需要 S3 的内核级 capability。
