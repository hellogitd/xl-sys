# XL-OS

> 下一代操作系统的定义特征，不是「更安全、更快、更好看」，而是——
> **它的基本抽象是为 AI 代理设计的，不是为人操作设计的。**

## 现状

**S0.1 进行中。** 九份设计文档完成，`xl-taskd` 的数据层已跑通。

已实现：

| 模块 | 内容 | 状态 |
|---|---|---|
| `taskd-core` | 任务 / 工作单元 / 步骤 / 委托 / 溯源的数据模型 | ✅ 17 个单元测试 |
| `taskd-store` | SQLite 持久化（步骤仅追加，任务可恢复） | ✅ 6 个单元测试 |
| `taskd-mcp` | MCP server（stdio + JSON-RPC），9 个工具 | ✅ 12 个单元测试 |
| `xl-taskd` CLI | 任务 / 工作单元 / 委托的完整命令行 | ✅ |

**委托检查（S0.2）已生效**：带 `target` + `action` 的步骤会先查委托，未授权则暂停任务并提示申请方式。agent 不能自己批准委托——批准是人的动作（`xl-taskd approve`）。

未实现：撤销引擎（S0.3）、溯源传播（S0.4）。

MCP 接入方式见 [docs/MCP-SETUP.md](docs/MCP-SETUP.md)。

### 快速试用

```bash
cargo build
./target/debug/xl-taskd begin "整理下载文件夹里的截图"
./target/debug/xl-taskd step <任务编号> "列出下载目录中的 png 文件"
./target/debug/xl-taskd status <任务编号>
./target/debug/xl-taskd list
```

数据库默认在 `./taskd.db`，可用 `XLTASKD_DB` 环境变量指定。

### 开发

```bash
cargo test        # 跑全部单元测试
cargo build       # 构建
```

## 核心观点

现有操作系统的抽象都建立在一个假设上：**用户是个人，手动操作**。当 AI 代理成为一等公民，这些抽象都表达不了它需要的东西：

| 旧抽象 | 它假设什么 | 对 AI 代理为什么错 |
|---|---|---|
| 进程 | 人启动程序，跑完退出 | 需要**可撤销、可审计、可中断的操作序列** |
| 文件 | 人创建、保存、关闭 | 工作连续、增量、多代理并行；文件树表达不了状态 |
| 权限 | 区分「我 / 别人 / root」 | 需要**按任务、按时限、按范围的委托** |
| 窗口 | 人盯着屏幕操作 | 交付的是**结果 + 可解释的过程** |

所以我们要重做的是：**任务（Task）、委托（Delegation）、撤销（Undo）、溯源（Taint）**。

## 路线

| 阶段 | 时间 | 内容 | 验证什么 |
|---|---|---|---|
| **S0** | 0–5 月 | 在现有系统上做 MCP 运行时 | 抽象对不对 |
| **S1** | 5–10 月 | 开发者 SDK，跑真实负载 | 有没有人愿意用 |
| **S2** | 10–18 月 | 中文关键字语言前端 + 双模界面原型 | 交互模型对不对 |
| **S3** | 18–42 月 | 自研 Rust 微内核 | 落地 |
| **S4** | 42+ 月 | Linux ABI 兼容 | 继承生态 |

**内核是第四步，不是第一步。** 先验证抽象，再造内核——这是本项目与大多数 hobby OS 最不同的地方。

内核必须自研的唯一理由：**用户态拦截可以被绕过**（agent 能执行任意 shell，委托检查就是摆设）。真正的隔离需要内核级 capability。

## 文档

| 文档 | 内容 |
|---|---|
| [MANIFESTO.md](MANIFESTO.md) | 为什么做这个 · 六个核心抽象 · 可证伪的实验 |
| [POSITIONING.md](POSITIONING.md) | 四十年微内核史的死因 · 为什么是现在 |
| [BLUEPRINT.md](BLUEPRINT.md) | 建成后什么样（UI / 设备 / 云端 / 官网 / 社区 / 捐赠） |
| [ARCHITECTURE.md](ARCHITECTURE.md) | 系统设计 · 兼容层 · 里程碑 |
| [DECISIONS.md](DECISIONS.md) | 每条决策及其推翻条件（ADR） |
| [S0-LOAD.md](S0-LOAD.md) | 三个真实负载反推的需求 |
| [S0-DESIGN.md](S0-DESIGN.md) | S0 的技术实现 |
| [HARDWARE.md](HARDWARE.md) | 硬件资产与用途 |
| [REFERENCES.md](REFERENCES.md) | 参考项目与许可红线 |

## 许可

[MIT](LICENSE)

## 状态诚实说明

这个项目目前**有一个能跑的任务记录 CLI，但还不能撤销、不能拦截越权、没有 MCP 接口**。

它离"可用的系统"还很远。如果你在评估是否值得关注，请先读 [MANIFESTO.md](MANIFESTO.md) 第 5 节——那里有四个可证伪的实验和明确的失败标准。**被证伪的宣言比没被检验的信念有价值。**
