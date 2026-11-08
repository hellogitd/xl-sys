//! SQLite 持久化层。
//!
//! 设计要点：
//! - `steps` 只追加，不更新（审计独立性）
//! - 任务状态持久化后，进程崩溃不会丢任务（S0-DESIGN 1.3）
//! - JSON 字段用 TEXT 存，方便直接 `sqlite3` 查看

use rusqlite::{params, Connection, OptionalExtension};
use taskd_core::*;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("序列化错误: {0}")]
    Json(#[from] serde_json::Error),
    #[error("未找到: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS tasks (
    id             TEXT PRIMARY KEY,
    goal           TEXT NOT NULL,
    state          TEXT NOT NULL,
    parent         TEXT,
    failure_policy TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS work_units (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    kind        TEXT NOT NULL,
    checkpoint  TEXT,
    outcome     TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_units_task ON work_units(task_id);

CREATE TABLE IF NOT EXISTS steps (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    unit_id     TEXT REFERENCES work_units(id),
    idx         INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    intent      TEXT NOT NULL,
    effect_json TEXT,
    taint_json  TEXT NOT NULL,
    risk        TEXT NOT NULL,
    result_json TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    UNIQUE(task_id, idx)
);
CREATE INDEX IF NOT EXISTS idx_steps_task ON steps(task_id, idx);

CREATE TABLE IF NOT EXISTS delegations (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    parent      TEXT,
    granted_by  TEXT NOT NULL,
    scope_json  TEXT NOT NULL,
    actions_json TEXT NOT NULL,
    trust_level TEXT NOT NULL,
    expires_at  INTEGER,
    max_ops     INTEGER,
    ops_used    INTEGER NOT NULL DEFAULT 0,
    granted     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_deleg_task ON delegations(task_id);

CREATE TABLE IF NOT EXISTS access_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id     TEXT NOT NULL,
    delegation  TEXT,
    target      TEXT NOT NULL,
    action      TEXT NOT NULL,
    allowed     INTEGER NOT NULL,
    at          INTEGER NOT NULL
);
"#;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        let s = Self { conn };
        s.conn.execute_batch(SCHEMA)?;
        Ok(s)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let s = Self { conn };
        s.conn.execute_batch(SCHEMA)?;
        Ok(s)
    }

    // ------------------------------------------------------------ tasks

    pub fn create_task(&self, t: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks (id, goal, state, parent, failure_policy, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                t.id,
                t.goal,
                serde_json::to_string(&t.state)?,
                t.parent,
                serde_json::to_string(&t.failure_policy)?,
                t.created_at,
                t.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, goal, state, parent, failure_policy, created_at, updated_at
                 FROM tasks WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;

        row.map(|(id, goal, state, parent, fp, ca, ua)| {
            Ok(Task {
                id,
                goal,
                state: serde_json::from_str(&state)?,
                parent,
                failure_policy: serde_json::from_str(&fp)?,
                created_at: ca,
                updated_at: ua,
            })
        })
        .transpose()
    }

    pub fn update_task_state(&self, id: &str, state: TaskState) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE tasks SET state = ?1, updated_at = ?2 WHERE id = ?3",
            params![serde_json::to_string(&state)?, now_ms(), id],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("task {id}")));
        }
        Ok(())
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, goal, state, parent, failure_policy, created_at, updated_at
             FROM tasks ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, goal, state, parent, fp, ca, ua) = row?;
            out.push(Task {
                id,
                goal,
                state: serde_json::from_str(&state)?,
                parent,
                failure_policy: serde_json::from_str(&fp)?,
                created_at: ca,
                updated_at: ua,
            });
        }
        Ok(out)
    }

    // ------------------------------------------------------------ steps

    pub fn append_step(&self, s: &Step) -> Result<()> {
        self.conn.execute(
            "INSERT INTO steps
             (id, task_id, unit_id, idx, kind, intent, effect_json, taint_json, risk, result_json, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                s.id,
                s.task_id,
                s.unit_id,
                s.index,
                serde_json::to_string(&s.kind)?,
                s.intent,
                s.effect.as_ref().map(serde_json::to_string).transpose()?,
                serde_json::to_string(&s.taint)?,
                serde_json::to_string(&s.risk)?,
                serde_json::to_string(&s.result)?,
                s.started_at,
                s.finished_at
            ],
        )?;
        Ok(())
    }

    pub fn list_steps(&self, task_id: &str) -> Result<Vec<Step>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, unit_id, idx, kind, intent, effect_json, taint_json, risk, result_json, started_at, finished_at
             FROM steps WHERE task_id = ?1 ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map(params![task_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, Option<i64>>(11)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, task_id, unit_id, index, kind, intent, effect, taint, risk, result, sa, fa) =
                row?;
            out.push(Step {
                id,
                task_id,
                unit_id,
                index,
                kind: serde_json::from_str(&kind)?,
                intent,
                effect: effect.map(|e| serde_json::from_str(&e)).transpose()?,
                taint: serde_json::from_str(&taint)?,
                risk: serde_json::from_str(&risk)?,
                result: serde_json::from_str(&result)?,
                started_at: sa,
                finished_at: fa,
            });
        }
        Ok(out)
    }

    /// 下一个 step index（append 用）。
    pub fn next_step_index(&self, task_id: &str) -> Result<u32> {
        let n: Option<u32> = self.conn.query_row(
            "SELECT MAX(idx) FROM steps WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )?;
        Ok(n.map(|v| v + 1).unwrap_or(0))
    }

    /// 某个工作单元下的全部步骤（按 index 升序）。
    pub fn list_steps_by_unit(&self, unit_id: &str) -> Result<Vec<Step>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, unit_id, idx, kind, intent, effect_json, taint_json, risk, result_json, started_at, finished_at
             FROM steps WHERE unit_id = ?1 ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map(params![unit_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, Option<i64>>(11)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, task_id, unit_id, index, kind, intent, effect, taint, risk, result, sa, fa) =
                row?;
            out.push(Step {
                id,
                task_id,
                unit_id,
                index,
                kind: serde_json::from_str(&kind)?,
                intent,
                effect: effect.map(|e| serde_json::from_str(&e)).transpose()?,
                taint: serde_json::from_str(&taint)?,
                risk: serde_json::from_str(&risk)?,
                result: serde_json::from_str(&result)?,
                started_at: sa,
                finished_at: fa,
            });
        }
        Ok(out)
    }

    // ------------------------------------------------------------ work units

    pub fn create_unit(&self, u: &WorkUnit) -> Result<()> {
        self.conn.execute(
            "INSERT INTO work_units (id, task_id, kind, checkpoint, outcome, created_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                u.id,
                u.task_id,
                serde_json::to_string(&u.kind)?,
                u.checkpoint,
                serde_json::to_string(&u.outcome)?,
                u.created_at,
                u.finished_at
            ],
        )?;
        Ok(())
    }

    pub fn finish_unit(&self, id: &str, outcome: Outcome) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE work_units SET outcome = ?1, finished_at = ?2 WHERE id = ?3",
            params![serde_json::to_string(&outcome)?, now_ms(), id],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("unit {id}")));
        }
        Ok(())
    }

    pub fn list_units(&self, task_id: &str) -> Result<Vec<WorkUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, kind, checkpoint, outcome, created_at, finished_at
             FROM work_units WHERE task_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![task_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, task_id, kind, checkpoint, outcome, ca, fa) = row?;
            out.push(WorkUnit {
                id,
                task_id,
                kind: serde_json::from_str(&kind)?,
                checkpoint,
                outcome: serde_json::from_str(&outcome)?,
                created_at: ca,
                finished_at: fa,
            });
        }
        Ok(out)
    }

    // ------------------------------------------------------------ delegations

    pub fn save_delegation(&self, d: &Delegation) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO delegations
             (id, task_id, parent, granted_by, scope_json, actions_json, trust_level, expires_at, max_ops, ops_used, granted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                d.id,
                d.task_id,
                d.parent,
                serde_json::to_string(&d.granted_by)?,
                serde_json::to_string(&d.scope)?,
                serde_json::to_string(&d.actions)?,
                serde_json::to_string(&d.trust_level)?,
                d.expires_at,
                d.max_ops,
                d.ops_used,
                d.granted as i32
            ],
        )?;
        Ok(())
    }

    pub fn get_delegation(&self, id: &str) -> Result<Option<Delegation>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, task_id, parent, granted_by, scope_json, actions_json, trust_level, expires_at, max_ops, ops_used, granted
                 FROM delegations WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<i64>>(7)?,
                        r.get::<_, Option<u64>>(8)?,
                        r.get::<_, u64>(9)?,
                        r.get::<_, i32>(10)?,
                    ))
                },
            )
            .optional()?;

        row.map(
            |(id, task_id, parent, gb, scope, actions, trust, exp, max_ops, used, granted)| {
                Ok(Delegation {
                    id,
                    task_id,
                    parent,
                    granted_by: serde_json::from_str(&gb)?,
                    scope: serde_json::from_str(&scope)?,
                    actions: serde_json::from_str(&actions)?,
                    trust_level: serde_json::from_str(&trust)?,
                    expires_at: exp,
                    max_ops,
                    ops_used: used,
                    granted: granted != 0,
                })
            },
        )
        .transpose()
    }

    pub fn list_delegations(&self, task_id: &str) -> Result<Vec<Delegation>> {
        let ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM delegations WHERE task_id = ?1 ORDER BY rowid ASC")?;
            let rows = stmt.query_map(params![task_id], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(d) = self.get_delegation(&id)? {
                out.push(d);
            }
        }
        Ok(out)
    }

    /// 批准一个委托（人类动作，不经 MCP 暴露给 agent）。
    pub fn grant_delegation(&self, id: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE delegations SET granted = 1 WHERE id = ?1",
            params![id],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("delegation {id}")));
        }
        Ok(())
    }

    /// 记录一次使用（ops_used += 1）。
    pub fn touch_delegation(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE delegations SET ops_used = ops_used + 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn log_access(
        &self,
        task_id: &str,
        delegation: Option<&str>,
        target: &str,
        action: Action,
        allowed: bool,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO access_log (task_id, delegation, target, action, allowed, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                task_id,
                delegation,
                target,
                serde_json::to_string(&action)?,
                allowed as i32,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// 统计审计记录条数（测试与 UI 用）。
    pub fn audit_count(&self, task_id: &str, allowed: bool) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM access_log WHERE task_id = ?1 AND allowed = ?2",
            params![task_id, allowed as i32],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// 列出任务的委托（按创建顺序）。
    pub fn list_delegations_for_task(&self, task_id: &str) -> Result<Vec<Delegation>> {
        self.list_delegations(task_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let mut t = Task::new("整理下载文件夹");
        s.create_task(&t).unwrap();
        t.transition_to(TaskState::Running).unwrap();
        s.update_task_state(&t.id, TaskState::Running).unwrap();

        let got = s.get_task(&t.id).unwrap().unwrap();
        assert_eq!(got.goal, "整理下载文件夹");
        assert_eq!(got.state, TaskState::Running);
    }

    #[test]
    fn steps_are_append_only_and_ordered() {
        let s = Store::open_in_memory().unwrap();
        let t = Task::new("写一个小工具");
        s.create_task(&t).unwrap();

        for i in 0..3 {
            let idx = s.next_step_index(&t.id).unwrap();
            assert_eq!(idx, i);
            let mut st = Step::new(&t.id, idx, StepKind::ToolCall, format!("第 {i} 步"));
            st.finish(StepResult::Ok { payload: None });
            s.append_step(&st).unwrap();
        }

        let steps = s.list_steps(&t.id).unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[2].intent, "第 2 步");
    }

    #[test]
    fn duplicate_step_index_rejected() {
        let s = Store::open_in_memory().unwrap();
        let t = Task::new("x");
        s.create_task(&t).unwrap();
        s.append_step(&Step::new(&t.id, 0, StepKind::Decision, "a"))
            .unwrap();
        // 同一 index 再插应失败（UNIQUE(task_id, idx)）
        assert!(s
            .append_step(&Step::new(&t.id, 0, StepKind::Decision, "b"))
            .is_err());
    }

    #[test]
    fn work_unit_lifecycle() {
        let s = Store::open_in_memory().unwrap();
        let t = Task::new("尝试写代码");
        s.create_task(&t).unwrap();

        let mut u = WorkUnit::new(&t.id, WorkUnitKind::Attempt);
        s.create_unit(&u).unwrap();
        u.finish(Outcome::Failed);
        s.finish_unit(&u.id, Outcome::Failed).unwrap();

        let units = s.list_units(&t.id).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].outcome, Outcome::Failed);
    }

    #[test]
    fn delegation_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let t = Task::new("整理照片");
        s.create_task(&t).unwrap();
        let d = Delegation::new(
            &t.id,
            Principal::User,
            vec!["/home/u/照片".into()],
            vec![Action::Read, Action::Write],
            TrustLevel::L1Restricted,
        );
        s.save_delegation(&d).unwrap();
        let got = s.get_delegation(&d.id).unwrap().unwrap();
        assert_eq!(got.scope, vec!["/home/u/照片".to_string()]);
    }

    #[test]
    fn missing_task_returns_none() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.get_task("task_nope").unwrap().is_none());
    }
}
