//! XL-OS `taskd` 核心模型：任务 / 委托 / 撤销 / 溯源。
//!
//! 对应 [S0-DESIGN.md](../../../S0-DESIGN.md) 第 1–4 节。
//! 这个 crate 只放数据结构与纯逻辑，不做 I/O、不做协议。

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- ID / 时间

pub type TaskId = String;
pub type StepId = String;
pub type WorkUnitId = String;
pub type DelegationId = String;

/// 生成带前缀的 ULID（按时间可排序）。
pub fn new_id(prefix: &str) -> String {
    format!("{}_{}", prefix, ulid::Ulid::new())
}

/// 当前 Unix 毫秒时间戳。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------- 错误

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CoreError {
    #[error("任务状态不能从 {from:?} 转换到 {to:?}")]
    InvalidTransition { from: TaskState, to: TaskState },
    #[error("子委托不能放大父委托的权限：{0}")]
    DelegationAmplification(String),
    #[error("委托已过期")]
    DelegationExpired,
    #[error("委托次数已用尽")]
    DelegationExhausted,
}

// ---------------------------------------------------------------- 任务

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    RolledBack,
    /// 已部署，长期运行（S0-DESIGN 1.4）
    Deployed,
}

impl TaskState {
    /// 终态：不再有后续操作。
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskState::Completed | TaskState::RolledBack)
    }

    /// 是否允许从 `self` 转换到 `to`。
    pub fn can_transition_to(self, to: TaskState) -> bool {
        use TaskState::*;
        matches!(
            (self, to),
            (Pending, Running)
                | (Pending, Failed)
                | (Running, Paused)
                | (Running, Completed)
                | (Running, Failed)
                | (Running, Deployed)
                | (Paused, Running)
                | (Paused, RolledBack)
                | (Failed, RolledBack)
                | (Deployed, RolledBack)
                | (Deployed, Running)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    /// 失败即回滚本工作单元
    Rollback,
    /// 失败即暂停，等人处理
    Pause,
    /// 记录后继续
    Continue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub goal: String,
    pub state: TaskState,
    pub parent: Option<TaskId>,
    pub failure_policy: FailurePolicy,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Task {
    pub fn new(goal: impl Into<String>) -> Self {
        let now = now_ms();
        Self {
            id: new_id("task"),
            goal: goal.into(),
            state: TaskState::Pending,
            parent: None,
            failure_policy: FailurePolicy::Pause,
            created_at: now,
            updated_at: now,
        }
    }

    /// 状态转换，非法转换返回错误（不可静默失败）。
    pub fn transition_to(&mut self, to: TaskState) -> Result<(), CoreError> {
        if !self.state.can_transition_to(to) {
            return Err(CoreError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        self.state = to;
        self.updated_at = now_ms();
        Ok(())
    }
}

// ---------------------------------------------------------------- 工作单元

/// 撤销的粒度单位（S0-DESIGN 1.1 / D015）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkUnitKind {
    /// 一次试错：agent 可以反复失败重试
    Attempt,
    /// 一个变更：如安装一个软件
    Change,
    /// 一个可交付版本
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Running,
    Succeeded,
    Failed,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkUnit {
    pub id: WorkUnitId,
    pub task_id: TaskId,
    pub kind: WorkUnitKind,
    /// 本单元起点的检查点（撤销用）
    pub checkpoint: Option<String>,
    pub outcome: Outcome,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

impl WorkUnit {
    pub fn new(task_id: impl Into<TaskId>, kind: WorkUnitKind) -> Self {
        Self {
            id: new_id("unit"),
            task_id: task_id.into(),
            kind,
            checkpoint: None,
            outcome: Outcome::Running,
            created_at: now_ms(),
            finished_at: None,
        }
    }

    pub fn finish(&mut self, outcome: Outcome) {
        self.outcome = outcome;
        self.finished_at = Some(now_ms());
    }
}

// ---------------------------------------------------------------- 步骤

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    ToolCall,
    Decision,
    UserInput,
    SubTask,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepResult {
    Pending,
    Ok { payload: Option<String> },
    Err { kind: String, message: String },
}

impl StepResult {
    pub fn is_err(&self) -> bool {
        matches!(self, StepResult::Err { .. })
    }
}

/// 副作用的四种可逆性（S0-DESIGN 3.1 / D015）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    /// 文件/目录：快照可完整恢复
    SnapshotRef { root: String, snapshot: String },
    /// 应用状态：逆操作
    InverseOp { op: String, inverse: String },
    /// 物理动作：不可撤销，但可补偿
    Compensable { action: String, compensation: String },
    /// 真的无法挽回
    Irreversible { reason: String },
}

/// 风险四级（S0-DESIGN 3.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// 可撤销
    R0,
    /// 可补偿
    R1,
    /// 不可逆，需事前确认
    R2,
    /// 危险，需沙箱预演 + 多重确认
    R3,
}

impl Effect {
    /// 由副作用类型推导默认风险级。
    pub fn risk_level(&self) -> RiskLevel {
        match self {
            Effect::SnapshotRef { .. } | Effect::InverseOp { .. } => RiskLevel::R0,
            Effect::Compensable { .. } => RiskLevel::R1,
            Effect::Irreversible { .. } => RiskLevel::R2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: StepId,
    pub task_id: TaskId,
    pub unit_id: Option<WorkUnitId>,
    /// 单调递增，从 0 开始
    pub index: u32,
    pub kind: StepKind,
    /// 这一步打算干什么（自然语言）。必填——保证系统永远能回答「为什么」。
    pub intent: String,
    pub effect: Option<Effect>,
    pub taint: TaintSet,
    pub risk: RiskLevel,
    pub result: StepResult,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl Step {
    pub fn new(
        task_id: impl Into<TaskId>,
        index: u32,
        kind: StepKind,
        intent: impl Into<String>,
    ) -> Self {
        Self {
            id: new_id("step"),
            task_id: task_id.into(),
            unit_id: None,
            index,
            kind,
            intent: intent.into(),
            effect: None,
            taint: TaintSet::default(),
            risk: RiskLevel::R0,
            result: StepResult::Pending,
            started_at: now_ms(),
            finished_at: None,
        }
    }

    pub fn with_unit(mut self, unit_id: impl Into<WorkUnitId>) -> Self {
        self.unit_id = Some(unit_id.into());
        self
    }

    pub fn with_effect(mut self, effect: Effect) -> Self {
        self.risk = effect.risk_level();
        self.effect = Some(effect);
        self
    }

    pub fn with_taint(mut self, taint: TaintSet) -> Self {
        self.taint = taint;
        self
    }

    pub fn finish(&mut self, result: StepResult) {
        self.result = result;
        self.finished_at = Some(now_ms());
    }
}

// ---------------------------------------------------------------- 溯源

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalKind {
    Web,
    Mail,
    Api,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaintSource {
    /// 用户直接输入
    User,
    /// 本地可信文件
    LocalTrusted { path: String },
    /// 本地不可信位置（下载目录等）
    LocalUntrusted { path: String },
    /// 外部来源
    External {
        external_kind: ExternalKind,
        origin: String,
    },
    /// 设备输入（传感器/串口），同样不可信
    ExternalDevice { device_kind: String, id: String },
    /// 模型生成内容
    AiGenerated,
}

impl TaintSource {
    /// 是否属于不可信来源（触发写操作时需要确认）。
    pub fn is_untrusted(&self) -> bool {
        !matches!(self, TaintSource::User | TaintSource::LocalTrusted { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            TaintSource::User => "用户输入".into(),
            TaintSource::LocalTrusted { path } => format!("本地可信 {path}"),
            TaintSource::LocalUntrusted { path } => format!("下载目录 {path}"),
            TaintSource::External {
                external_kind,
                origin,
            } => match external_kind {
                ExternalKind::Web => format!("网页 {origin}"),
                ExternalKind::Mail => format!("邮件 {origin}"),
                ExternalKind::Api => format!("接口 {origin}"),
            },
            TaintSource::ExternalDevice { device_kind, id } => {
                format!("设备 {device_kind}/{id}")
            }
            TaintSource::AiGenerated => "模型生成".into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaintSet {
    sources: Vec<TaintSource>,
}

impl TaintSet {
    pub fn from_source(s: TaintSource) -> Self {
        Self { sources: vec![s] }
    }

    pub fn add(&mut self, s: TaintSource) {
        if !self.sources.contains(&s) {
            self.sources.push(s);
        }
    }

    pub fn merge(&mut self, other: &TaintSet) {
        for s in &other.sources {
            self.add(s.clone());
        }
    }

    pub fn sources(&self) -> &[TaintSource] {
        &self.sources
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn has_untrusted(&self) -> bool {
        self.sources.iter().any(|s| s.is_untrusted())
    }

    pub fn untrusted(&self) -> impl Iterator<Item = &TaintSource> {
        self.sources.iter().filter(|s| s.is_untrusted())
    }
}

// ---------------------------------------------------------------- 委托

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Write,
    Delete,
    Execute,
    Net,
    Device,
}

/// 渐进式授权级别（S0-DESIGN 2.4）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    L0Sandbox,
    L1Restricted,
    L2Normal,
    L3Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    User,
    Agent { task_id: TaskId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegation {
    pub id: DelegationId,
    pub task_id: TaskId,
    pub parent: Option<DelegationId>,
    pub granted_by: Principal,
    /// 路径 glob / URL 域名 / 设备 ID
    pub scope: Vec<String>,
    pub actions: Vec<Action>,
    pub trust_level: TrustLevel,
    pub expires_at: Option<i64>,
    pub max_ops: Option<u64>,
    pub ops_used: u64,
    /// 是否已被人类批准。agent 只能创建请求（false），批准是人的动作。
    pub granted: bool,
}

/// 简单匹配：`*` = 全部，`前缀*` = 前缀匹配，目录 = 覆盖其下所有路径，否则精确匹配。
fn pattern_matches(pattern: &str, target: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return target.starts_with(prefix);
    }
    if pattern == target {
        return true;
    }
    // 目录包含：target 位于 pattern 目录之下
    is_under(target, pattern)
}

/// `path` 是否严格位于 `dir` 之下（边界必须是 `/`）。
fn is_under(path: &str, dir: &str) -> bool {
    let dir = dir.strip_suffix('/').unwrap_or(dir);
    path.starts_with(dir) && path[dir.len()..].starts_with('/')
}

impl Delegation {
    pub fn new(
        task_id: impl Into<TaskId>,
        granted_by: Principal,
        scope: Vec<String>,
        actions: Vec<Action>,
        trust_level: TrustLevel,
    ) -> Self {
        Self {
            id: new_id("deleg"),
            task_id: task_id.into(),
            parent: None,
            granted_by,
            scope,
            actions,
            trust_level,
            expires_at: None,
            max_ops: None,
            ops_used: 0,
            granted: false,
        }
    }

    pub fn is_expired(&self, now: i64) -> bool {
        matches!(self.expires_at, Some(t) if now >= t)
    }

    pub fn is_exhausted(&self) -> bool {
        matches!(self.max_ops, Some(max) if self.ops_used >= max)
    }

    /// 此刻是否可用：已批准 + 未过期 + 未耗尽。
    pub fn is_active(&self, now: i64) -> bool {
        self.granted && !self.is_expired(now) && !self.is_exhausted()
    }

    /// 本次操作是否被本委托覆盖。
    pub fn covers(&self, target: &str, action: Action) -> bool {
        self.actions.contains(&action) && self.scope.iter().any(|p| pattern_matches(p, target))
    }

    /// 子委托是否合法（P-1：不可放大）。
    pub fn is_subset_of(&self, parent: &Delegation) -> bool {
        // 动作子集
        let actions_ok = self.actions.iter().all(|a| parent.actions.contains(a));
        // 信任级不超
        let trust_ok = self.trust_level <= parent.trust_level;
        // 每个 scope 都被父覆盖
        let scope_ok = self
            .scope
            .iter()
            .all(|child_pat| parent.scope.iter().any(|p| pattern_covers(p, child_pat)));
        actions_ok && trust_ok && scope_ok
    }
}

/// 父 pattern 是否覆盖子 pattern（保守实现：通配与目录包含，不做反向放大）。
fn pattern_covers(parent: &str, child: &str) -> bool {
    if parent == "*" {
        return true;
    }
    if parent == child {
        return true;
    }
    match (parent.strip_suffix('*'), child.strip_suffix('*')) {
        // 父是通配、子也是通配：子前缀必须在父前缀之下
        (Some(pp), Some(cp)) => cp.starts_with(pp),
        // 父是通配、子是精确：子路径必须在父前缀之下
        (Some(pp), None) => child.starts_with(pp),
        // 父是精确目录、子是通配：子的前缀必须严格在父目录之下
        (None, Some(cp)) => is_under(cp, parent),
        // 父是目录、子在目录之下
        (None, None) => is_under(child, parent),
    }
}

// ---------------------------------------------------------------- 授权检查

/// 授权检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// 被某个有效委托覆盖
    Allowed { delegation: DelegationId },
    /// 没有有效委托覆盖该操作
    NeedsPermission,
}

/// 在一组委托中查找能覆盖本次操作的委托。
///
/// 只有「已批准 + 未过期 + 未耗尽」的委托才算数（不变量 P-2、P-3）。
pub fn check_delegations(
    delegations: &[Delegation],
    target: &str,
    action: Action,
    now: i64,
) -> CheckOutcome {
    delegations
        .iter()
        .filter(|d| d.is_active(now))
        .find(|d| d.covers(target, action))
        .map(|d| CheckOutcome::Allowed {
            delegation: d.id.clone(),
        })
        .unwrap_or(CheckOutcome::NeedsPermission)
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_state_transitions() {
        let mut t = Task::new("整理下载文件夹");
        assert_eq!(t.state, TaskState::Pending);
        t.transition_to(TaskState::Running).unwrap();
        assert!(t.transition_to(TaskState::Completed).is_ok());
        assert!(t.state.is_terminal());
        // 终态不可再转
        assert!(t.transition_to(TaskState::Running).is_err());
    }

    #[test]
    fn illegal_transition_is_not_silent() {
        let mut t = Task::new("x");
        // Pending 不能直接 Completed
        let err = t.transition_to(TaskState::Completed).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTransition { .. }));
        assert_eq!(t.state, TaskState::Pending);
    }

    #[test]
    fn effect_risk_levels() {
        let e = Effect::SnapshotRef {
            root: "/home/u/a".into(),
            snapshot: "snap1".into(),
        };
        assert_eq!(e.risk_level(), RiskLevel::R0);

        let c = Effect::Compensable {
            action: "led_on".into(),
            compensation: "led_off".into(),
        };
        assert_eq!(c.risk_level(), RiskLevel::R1);

        let i = Effect::Irreversible {
            reason: "发送邮件".into(),
        };
        assert_eq!(i.risk_level(), RiskLevel::R2);
    }

    #[test]
    fn step_requires_intent() {
        let s = Step::new("task_1", 0, StepKind::ToolCall, "列出下载目录");
        assert_eq!(s.intent, "列出下载目录");
        assert_eq!(s.risk, RiskLevel::R0);
    }

    #[test]
    fn step_effect_updates_risk() {
        let s = Step::new("task_1", 0, StepKind::ToolCall, "开灯").with_effect(Effect::Compensable {
            action: "led_on".into(),
            compensation: "led_off".into(),
        });
        assert_eq!(s.risk, RiskLevel::R1);
    }

    #[test]
    fn taint_merge_and_untrusted() {
        let mut t = TaintSet::from_source(TaintSource::User);
        t.add(TaintSource::LocalTrusted {
            path: "/home/u/a".into(),
        });
        assert!(!t.has_untrusted());

        let ext = TaintSet::from_source(TaintSource::External {
            external_kind: ExternalKind::Web,
            origin: "https://example.com".into(),
        });
        t.merge(&ext);
        assert!(t.has_untrusted());
        assert_eq!(t.untrusted().count(), 1);
    }

    #[test]
    fn device_input_is_untrusted() {
        let t = TaintSet::from_source(TaintSource::ExternalDevice {
            device_kind: "esp32".into(),
            id: "ttyUSB0".into(),
        });
        assert!(t.has_untrusted());
    }

    #[test]
    fn delegation_covers_scope() {
        let d = Delegation::new(
            "task_1",
            Principal::User,
            vec!["/home/u/下载".into()],
            vec![Action::Read, Action::Write],
            TrustLevel::L1Restricted,
        );
        assert!(d.covers("/home/u/下载/a.png", Action::Read));
        assert!(!d.covers("/home/u/下载/a.png", Action::Delete));
        assert!(!d.covers("/home/u/密码.txt", Action::Read));
    }

    #[test]
    fn delegation_wildcard_scope() {
        let d = Delegation::new(
            "t",
            Principal::User,
            vec!["/home/u/下载/*".into()],
            vec![Action::Read],
            TrustLevel::L1Restricted,
        );
        assert!(d.covers("/home/u/下载/a.png", Action::Read));
        assert!(!d.covers("/home/u/照片/a.png", Action::Read));
    }

    #[test]
    fn delegation_expiry_and_exhaustion() {
        let mut d = Delegation::new(
            "t",
            Principal::User,
            vec!["*".into()],
            vec![Action::Read],
            TrustLevel::L1Restricted,
        );
        d.expires_at = Some(1000);
        assert!(d.is_expired(2000));
        assert!(!d.is_expired(500));

        d.max_ops = Some(2);
        d.ops_used = 2;
        assert!(d.is_exhausted());
    }

    #[test]
    fn child_delegation_cannot_amplify() {
        let parent = Delegation::new(
            "t",
            Principal::User,
            vec!["/home/u/下载".into()],
            vec![Action::Read],
            TrustLevel::L1Restricted,
        );

        // 合法子委托：范围更小、动作更少
        let ok = Delegation::new(
            "t",
            Principal::User,
            vec!["/home/u/下载/截图".into()],
            vec![Action::Read],
            TrustLevel::L0Sandbox,
        );
        assert!(ok.is_subset_of(&parent));

        // 非法：动作放大
        let bad_action = Delegation::new(
            "t",
            Principal::User,
            vec!["/home/u/下载".into()],
            vec![Action::Read, Action::Delete],
            TrustLevel::L1Restricted,
        );
        assert!(!bad_action.is_subset_of(&parent));

        // 非法：范围放大
        let bad_scope = Delegation::new(
            "t",
            Principal::User,
            vec!["/home/u".into()],
            vec![Action::Read],
            TrustLevel::L1Restricted,
        );
        assert!(!bad_scope.is_subset_of(&parent));

        // 非法：信任级放大
        let bad_trust = Delegation::new(
            "t",
            Principal::User,
            vec!["/home/u/下载".into()],
            vec![Action::Read],
            TrustLevel::L3Full,
        );
        assert!(!bad_trust.is_subset_of(&parent));
    }

    #[test]
    fn ungranted_delegation_never_allows() {
        let now = now_ms();
        let d = Delegation::new(
            "t",
            Principal::User,
            vec!["/home/u/下载".into()],
            vec![Action::Read],
            TrustLevel::L1Restricted,
        );
        assert_eq!(
            check_delegations(&[d], "/home/u/下载/a.png", Action::Read, now),
            CheckOutcome::NeedsPermission
        );
    }

    #[test]
    fn granted_delegation_allows_operation() {
        let now = now_ms();
        let mut d = Delegation::new(
            "t",
            Principal::User,
            vec!["/home/u/下载".into()],
            vec![Action::Read],
            TrustLevel::L1Restricted,
        );
        d.granted = true;
        match check_delegations(&[d], "/home/u/下载/a.png", Action::Read, now) {
            CheckOutcome::Allowed { delegation } => assert!(delegation.starts_with("deleg_")),
            other => panic!("expected allowed, got {other:?}"),
        }
    }

    #[test]
    fn expired_or_exhausted_delegation_is_inactive() {
        let now = now_ms();
        let mut d = Delegation::new(
            "t",
            Principal::User,
            vec!["*".into()],
            vec![Action::Read],
            TrustLevel::L1Restricted,
        );
        d.granted = true;
        d.expires_at = Some(now - 1);
        assert_eq!(
            check_delegations(std::slice::from_ref(&d), "/x", Action::Read, now),
            CheckOutcome::NeedsPermission
        );

        d.expires_at = None;
        d.max_ops = Some(1);
        d.ops_used = 1;
        assert_eq!(
            check_delegations(&[d], "/x", Action::Read, now),
            CheckOutcome::NeedsPermission
        );
    }

    #[test]
    fn action_outside_scope_needs_permission() {
        let now = now_ms();
        let mut d = Delegation::new(
            "t",
            Principal::User,
            vec!["/home/u/下载".into()],
            vec![Action::Read],
            TrustLevel::L1Restricted,
        );
        d.granted = true;
        // 动作不在委托里
        assert_eq!(
            check_delegations(std::slice::from_ref(&d), "/home/u/下载/a.png", Action::Delete, now),
            CheckOutcome::NeedsPermission
        );
        // 路径不在委托里
        assert_eq!(
            check_delegations(&[d], "/home/u/密码.txt", Action::Read, now),
            CheckOutcome::NeedsPermission
        );
    }

    #[test]
    fn serde_roundtrip() {
        let mut t = Task::new("测试目标");
        t.transition_to(TaskState::Running).unwrap();
        let json = serde_json::to_string(&t).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, t.id);
        assert_eq!(back.state, TaskState::Running);
    }

    #[test]
    fn effect_serde_tagged() {
        let e = Effect::Compensable {
            action: "led_on".into(),
            compensation: "led_off".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"compensable\""));
    }
}
