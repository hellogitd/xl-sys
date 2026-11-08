# S0 技术设计：任务 / 委托 / 撤销 / 溯源

> 状态：**v0.3** · 2026-11-08
> v0.2 修正来源：[S0-LOAD.md](S0-LOAD.md)（三个真实负载反推）
> v0.3 修正来源：硬件确认（ESP32 取代树莓派）+ 设备输入纳入溯源
> 对应 [MANIFESTO.md](MANIFESTO.md) 第 4–5 节、[DECISIONS.md](DECISIONS.md) D011/D012/D015
> 目标：**在现有操作系统上实现并验证四个新抽象，不写内核。**

---

## 0. 一个关键决定：先做成协议，再做成内核

S0 的产物不是"迷你 OS"，是一个**能被任何 agent 接入的运行时**。

形态：**MCP server**（`xl-taskd`），暴露任务、委托、撤销、溯源四组工具。任何支持 MCP 的 agent 都能接。

```
S0（现在）                    S3（自研内核）
xl-taskd（MCP 用户态）  ───▶  内核 capability + 用户态服务
       协议不变                    实现下沉
```

理由：不发明私有 API；抽象错了改协议而不是改内核；协议验证过的东西可以直接下沉。

---

## 1. 任务（Task）

### 1.1 数据结构

```rust
struct Task {
    id: TaskId,
    goal: String,                  // 用户目标原文
    state: TaskState,              // Pending/Running/Paused/Completed/Failed/RolledBack/Deployed
    parent: Option<TaskId>,
    work_units: Vec<WorkUnitId>,   // 【v0.2 新增】撤销粒度
    steps: Vec<Step>,              // append-only
    delegations: Vec<DelegationId>,
    failure_policy: FailurePolicy,
    created_at, updated_at: Timestamp,
}

// 【v0.2 新增】工作单元：撤销的粒度单位
struct WorkUnit {
    id: WorkUnitId,
    kind: WorkUnitKind,            // Attempt（一次试错）| Change（一个变更）| Release（一个可交付版本）
    steps: Vec<StepId>,
    checkpoint: CheckpointId,      // 可回到本单元起点
    outcome: Outcome,              // Succeeded | Failed | Abandoned
}

struct Step {
    index: u32,
    kind: StepKind,                // ToolCall | Decision | UserInput | SubTask | External
    intent: String,                // 【可解释性】这一步打算干什么
    effect: Option<Effect>,        // 【撤销】副作用（见第 3 节，v0.2 四类）
    taint: TaintSet,               // 【溯源】
    risk: RiskLevel,               // 【v0.2 新增】R0–R3
    result: StepResult,
    started_at, finished_at,
}
```

### 1.2 三条不可妥协的规则

1. **steps 只追加，不修改**——审计独立性来自历史不可篡改
2. **每个 Step 必须有 `intent`**——保证系统永远能回答"你为什么这么做"
3. **Task 不持有"结果"，只持有"历史"**——结果由历史推导，避免不一致

### 1.3 失败语义（v0.2 扩为三类）

| 失败类型 | 例子 | 处理 |
|---|---|---|
| **安全失败** | 装了恶意软件、越权成功 | 立即停止任务 + 强制审计 + 保留现场 |
| **质量失败** | 代码写错、测试不过、参数不对 | 回退到工作单元起点，重试（高频路径） |
| **物理失败** | 设备损坏、动作做错 | 执行补偿 + 人工介入 + 不可自动重试 |

**关键：进程崩溃 ≠ 任务崩溃。** 任务状态持久化在 SQLite，重启后能恢复现场。这是"任务"与"进程"的本质区别。

### 1.4 任务有第四种终态：Deployed

部署后的服务是长期运行的，任务本身已达成目标，但产生物还在跑，且随时可能需要停掉（= 一种特殊撤销）。

```rust
Deployed {
    services: Vec<ServiceRef>,     // 哪些服务在跑
    resources: Vec<ResourceRef>,   // 占用哪些端口 / 设备
    stop_procedure: Vec<Effect>,   // 如何安全停止（含补偿）
}
```

---

## 2. 委托（Delegation）

### 2.1 数据结构

```rust
struct Delegation {
    id: DelegationId,
    task_id: TaskId,
    granted_by: Principal,         // User | Agent(TaskId)
    parent: Option<DelegationId>,

    scope: Vec<ResourcePattern>,   // 路径 glob | URL 域名 | 设备 ID
    actions: BitFlags<Action>,     // Read/Write/Delete/Execute/Net/Device
    trust_level: TrustLevel,       // 【v0.2 新增】见 2.4
    expires_at: Option<Timestamp>,
    max_ops: Option<u64>,

    log: Vec<AccessRecord>,
}
```

### 2.2 安全性质（代码里必须断言）

| # | 性质 | 检查点 |
|---|---|---|
| P-1 | **子委托 ⊆ 父委托**（不可放大） | 创建委托时 |
| P-2 | **每次操作前检查**，不是创建时检查一次 | 每次工具调用 |
| P-3 | **任务结束，委托立即失效** | 任务状态变更 |
| P-4 | **越界 = 暂停 + 请求**，不是拒绝 + 继续 | 越界时 |
| P-5 | **所有访问入审计，可查询** | 每次操作后 |

### 2.3 在 S0 里怎么落地

S0 不拦截系统调用，只**拦截工具调用**：

```
agent 想执行操作
  → 只能经由 xl-taskd 提供的工具
  → 执行前查 Delegation
  → 通过则执行并记录；不通过则返回 NEEDS_PERMISSION
```

**已知漏洞（S0 明确接受）：** agent 若能执行任意 shell，就绕过委托检查。
对策：S0 的 agent **只能使用受控工具集，shell 默认关闭**（已确认接受）。必须开 shell 时，整个调用视为 R3 危险操作。

### 2.4 渐进式授权（v0.2 新增，来自 T1）

现有 OS 对"运行别人的代码"没有中间态：要么不跑，要么全权跑。中间应该是：

| 级别 | 权限 | 用途 |
|---|---|---|
| **L0 沙箱** | 无网络、无文件写入、只读挂载 | 行为预演 |
| **L1 受限** | 声明式权限，最小集 | 首次运行 |
| **L2 常规** | 用户确认后的完整权限 | 确认后 |
| **L3 完全** | 无限制 | 用户显式信任 |

**升级必须由用户逐级确认，不可自动跳级。**

---

## 3. 撤销（Undo）—— 最关键的技术决策

### 3.1 Effect 模型（v0.2 从两类扩为四类）

```rust
enum Effect {
    // 可完整撤销（文件/目录）
    SnapshotRef { root: PathBuf, snapshot: SnapshotId },

    // 可逆操作（应用状态）
    InverseOp { op: OpKind, inverse: InverseOpKind },

    // 【v0.2 新增】物理操作：不可撤销，但可补偿
    Compensable { action: Action, compensation: Action },

    // 真的无法挽回：必须事前确认
    Irreversible { reason: String },
}
```

**为什么需要第三类：** T3（硬件）暴露了"不可逆"这个二分法太粗。开灯不能撤销，但能关灯；电机转 10° 不能撤销，但能反转 10°。**大部分物理动作属于"可补偿"，不是"不可逆"。**

### 3.2 撤销的混合实现

| 层级 | 方案 | 平台支持 |
|---|---|---|
| 文件/目录 | 快照（reflink / clonefile / block clone） | btrfs/XFS、APFS、ReFS；ext4 用 `cp --reflink=auto` 退化 |
| 应用状态 | 逆操作 | 每种操作自己实现 |
| 物理动作 | 补偿动作 | 设备驱动定义 |
| 外部副作用 | 不可逆 + 事前确认 | 网络请求、发消息 |

### 3.3 撤销算法（按工作单元，不是按步骤）

```
undo_unit(task, unit_id):
    for effect in reverse(units[unit_id].effects()):
        match effect:
            SnapshotRef  → restore(snapshot)
            InverseOp    → apply_inverse(op)
            Compensable  → apply(compensation)
            Irreversible → fail("该操作不可撤销，需要人工介入")
    mark unit as Abandoned
```

### 3.4 性能阈值（v0.2 拆成两个）

| 场景 | 频率 | 阈值 | 实现 |
|---|---|---|---|
| **用户触发撤销** | 低 | < 5 秒 | 完整快照恢复 |
| **agent 试错撤销** | **高（几十次/任务）** | **< 200 毫秒** | 增量快照 / 内存层 |
| 回退到"能跑"版本 | 中 | < 2 秒 | 检查点 |

**这是 v0.1 的真实缺陷：** agent 每次试错撤销若需 5 秒，20 次试错 = 100 秒纯开销，agent 会绕过撤销自己想办法，设计就废了。

### 3.5 风险分级（v0.2 新增）

| 级别 | 例子 | 处理 |
|---|---|---|
| **R0 可撤销** | 写文件、改配置 | 直接执行 |
| **R1 可补偿** | 开关设备、发通知 | 执行 + 记录补偿 |
| **R2 不可逆** | 删除外部数据、发消息 | 事前确认 |
| **R3 危险** | 不可信代码执行、格式化 | 沙箱预演 + 多重确认 |

---

## 4. 溯源（Taint）

### 4.1 来源分级

```rust
enum TaintSource {
    User,                             // 用户直接输入（可信）
    LocalTrusted(PathBuf),            // 本地可信文件
    LocalUntrusted(PathBuf),          // 下载目录等
    External { kind: Web|Mail|Api, origin: String },  // 外部（不可信）
    ExternalDevice { kind, id },      // 【v0.3 新增】设备输入（传感器/串口），同样不可信
    AIGenerated,                      // 模型生成
}
```

**为什么设备输入也要标不可信：** 被攻破的设备、接线错误的传感器、脏数据，都会让 agent 把错误信息当事实。ESP32 场景的额外验收就是"脏串口数据不能带偏 agent"。

### 4.2 传播与触发

- **传播**：Step 读到的数据，来源并入 `Step.taint`
- **触发**：存在 `External` 来源时，写操作要求确认并展示来源

```
「这个操作基于 3 个外部来源：
   · https://example.com/article（网页）
   · 邮件「关于项目报价」
 是否继续？」
```

这就是**提示注入的防线**：注入内容来自外部，触发写操作时会被标出来。

### 4.3 S0 的粒度（诚实说明）

只做**文件级 / 数据源级**溯源，不做字节级数据流跟踪。字节级性能代价太大（实验 4 的失败标准就是它），S3 再评估。

---

## 5. 与 DSH 的集成

### 5.1 形态

`xl-taskd` 作为 MCP server，DSH 作为 MCP client。**不修改 DSH 内核**，只经 MCP 协议接入。

### 5.2 暴露的工具（协议的一部分，要慎重）

| 工具 | 用途 |
|---|---|
| `task_begin(goal)` | 开启任务 |
| `task_step(intent, tool, args)` | 记录并执行一步 |
| `task_status(task_id)` | 查询历史 |
| `unit_begin(kind)` / `unit_end(outcome)` | 工作单元边界 |
| `task_undo(task_id, to_unit)` | 回退到工作单元 |
| `task_pause / task_resume` | 暂停/恢复 |
| `delegation_request(task_id, scope, actions, trust, ttl)` | 申请委托 |
| `taint_report(task_id)` | 来源清单 |
| `service_deploy / service_stop` | 部署与停止 |

**工具数量要少，每加一个都是长期负担。**

### 5.3 一次完整调用

```
agent → task_begin(goal="整理下载文件夹里的截图，按日期归档")
      ← { task_id: 01J..., 需要委托: read:~/下载, write:~/下载/截图 }

agent → delegation_request(scope=["~/下载"], actions=[READ], trust=L1, ttl=600s)
用户  ← 确认
      ← { delegation_id: D1 }

agent → unit_begin(kind=Change)
agent → task_step(intent="列出 png 文件", tool="fs_list")     ← step 1
agent → task_step(intent="创建归档目录", tool="fs_mkdir")     ← step 2
agent → task_step(intent="移动文件", tool="fs_move")          ← step 3
      ← { effect: SnapshotRef(...) }
agent → unit_end(outcome=Succeeded)

用户  → "不对，整个重来"
agent → task_undo(task_id, to_unit=U1)
      ← { undone: 1 unit, elapsed: 0.18s }
```

---

## 6. S0.1–S0.5 任务拆解（对齐负载验收场景）

| 里程碑 | 技术内容 | 验收 | 预估 |
|---|---|---|---|
| **S0.1** | SQLite schema + Task/Step/WorkUnit + task_begin/step/status | 能记录完整操作序列并重放 | 3 周 |
| **S0.2** | Delegation + 渐进式授权 + 检查点 + 越界暂停 | 越权 100% 拦截，0 次静默放行 | 3 周 |
| **S0.3** | 四类 Effect + 快照引擎 + 双阈值撤销 | **用户撤销 < 5s，agent 试错撤销 < 200ms** | 6 周 |
| **S0.4** | Taint 标记 + 传播 + 写前确认 | 外部来源触发的写操作 100% 提示 | 3 周 |
| **S0.5** | 三个真实负载端到端（V1/V2/V3） | 见 [S0-LOAD.md](S0-LOAD.md) 第 5 节，三个场景全通过 | 5 周 |

合计约 **20 周（5 个月）**。

### S0.5 的三个验收场景（摘要）

| 场景 | 对应 | 验收 |
|---|---|---|
| **V1** | T1 装陌生应用并完整卸载 | 回滚后系统状态与安装前 diff 为空；用户只确认 2 次 |
| **V2** | T2 写工具，允许失败并回退 | 连续试错 10 次，每次撤销 < 200ms；工作区无中间垃圾 |
| **V3** | T3 硬件联动部署与停止 | 停止后无残留进程、设备释放、灯关闭；断线能重连 |

---

## 7. 技术选型

| 项 | 选择 | 理由 |
|---|---|---|
| 语言 | **Rust** | 与 S3 内核一致，代码可迁移；快照引擎要正确性 |
| 存储 | **SQLite** | 单文件、事务、嵌入式 |
| 快照 | 平台原生（reflink/clonefile/copy 降级） | 见 3.2 |
| 接口 | **MCP server**（stdio + JSON-RPC） | 标准协议，多 agent 可用 |
| 日志 | 结构化 JSONL + SQLite 索引 | 可 grep、可查询 |
| 测试 | 属性测试（proptest）+ 故障注入 | 撤销引擎正确性必须靠属性测试 |
| 硬件（V3） | **ESP32 + 光敏/热敏 + LED（用户已有）** | 设备本身可编程、可注入异常、成本为零 |

---

## 8. 已确认 / 待确认

### 已确认（2026-11-08）

| # | 事项 | 结果 |
|---|---|---|
| 1 | 三个真实负载 | T1 装陌生应用 / T2 写小软件 / T3 硬件联动 |
| 2 | 撤销阈值 | 接受（v0.2 拆为双阈值） |
| 3 | S0 阶段 shell 默认关闭 | 接受 |
| 4 | 首个接入 agent | DSH，且 **xl-taskd 接管全部工具调用**（事前拦截，非旁路审计） |
| 5 | T3 硬件 | **ESP32 + 光敏/热敏 + LED**（用户已有），树莓派不需要 |
| 6 | 开发/测试机 | **H81 + i3-4160T + 4G + 64G**，装 Linux 跑 xl-taskd；同时是 **S3 的第一台 x86_64 真机目标** |

### 仍待确认

| # | 事项 | 影响 |
|---|---|---|
| 1 | 是否接受 S0 总周期 20 周 | 排期 |
| 2 | i3-4160T 那台机器的 UEFI 支持情况（H81 部分主板只有 Legacy） | 影响 S3 真机路径，不影响 S0 |
| 3 | 是否给那台机器加一块 SSD（64G 编译内核会紧张） | 只在 S3 时需要 |

---

## 附录：为什么这个设计能在现有 OS 上跑

| 能力 | 依赖内核吗 | 说明 |
|---|---|---|
| 任务模型 | 否 | 纯用户态，SQLite |
| 委托检查 | 否 | 拦截工具调用 |
| 撤销 | 否 | 依赖文件系统 reflink |
| 溯源 | 否 | 工具层维护 |
| **防绕过（真隔离）** | **是** | **这是 S3 必须自研内核的唯一理由** |

**S0 能验证抽象，但不能提供强隔离。** 强隔离需要内核级 capability。
