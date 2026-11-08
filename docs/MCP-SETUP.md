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

在客户端里看到 `xl-taskd` 的 7 个工具即为接入成功：

| 工具 | 用途 |
|---|---|
| `task_begin` | 创建任务，返回 `task_id` |
| `task_step` | 记录一步（`intent` 必填） |
| `task_status` | 查历史 |
| `task_list` | 列任务 |
| `task_done` | 标记完成 |
| `unit_begin` | 开始工作单元（attempt/change/release） |
| `unit_end` | 结束工作单元 |

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
| 无委托检查 | 任何工具调用都会执行 | S0.2 |
| 无撤销 | 只能记录，不能回退 | S0.3 |
| 无溯源 | 不记录数据来源 | S0.4 |
| 无 effect 参数 | `task_step` 暂不接受副作用描述 | S0.3 |

**在 S0.2 完成前，不要用它做真实文件操作**——目前它只是个记录器，不拦截任何东西。
