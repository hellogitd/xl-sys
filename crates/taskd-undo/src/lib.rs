//! 快照引擎与撤销算法（S0.3）。
//!
//! 对应 [S0-DESIGN.md](../../../S0-DESIGN.md) 第 3 节。
//!
//! 设计要点：
//! - **文件级快照**：操作前把文件/目录复制到快照区，撤销时复制回来
//! - **新建文件记录为 `existed: false`**：撤销时删除，而不是恢复空内容
//! - **不追求 reflink 优化**：先做对，再测速；测出来不达标再优化（实验 1 就是干这个的）
//! - **不可撤销的效果不静默跳过**：报告里必须列出来

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use taskd_core::now_ms;
use taskd_store::Store;

#[derive(Debug, thiserror::Error)]
pub enum UndoError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("快照不存在: {0}")]
    SnapshotNotFound(String),
    #[error("元数据损坏: {0}")]
    BadMeta(String),
    #[error("存储错误: {0}")]
    Store(String),
    #[error("超出快照大小上限: {path} 为 {size} 字节，上限 {limit}")]
    TooLarge {
        path: String,
        size: u64,
        limit: u64,
    },
}

pub type Result<T> = std::result::Result<T, UndoError>;

/// 单次快照允许的最大体积（默认 256 MiB）。
pub const DEFAULT_MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub id: String,
    /// 被快照的原路径
    pub original: PathBuf,
    /// 快照时该路径是否存在
    pub existed: bool,
    pub kind: EntryKind,
    pub created_at: i64,
    /// 快照内容字节数
    pub bytes: u64,
}

pub struct SnapshotStore {
    root: PathBuf,
    max_bytes: u64,
}

impl SnapshotStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            max_bytes: DEFAULT_MAX_SNAPSHOT_BYTES,
        })
    }

    pub fn with_limit(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn data_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    /// 对路径做快照。路径不存在也合法（记录 `existed: false`）。
    pub fn capture(&self, path: &Path) -> Result<SnapshotMeta> {
        let id = taskd_core::new_id("snap");
        let exists = path.exists();

        if !exists {
            let meta = SnapshotMeta {
                id,
                original: path.to_path_buf(),
                existed: false,
                kind: EntryKind::Missing,
                created_at: now_ms(),
                bytes: 0,
            };
            self.write_meta(&meta)?;
            return Ok(meta);
        }

        let size = entry_size(path)?;
        if size > self.max_bytes {
            return Err(UndoError::TooLarge {
                path: path.display().to_string(),
                size,
                limit: self.max_bytes,
            });
        }

        let dest = self.data_dir(&id);
        fs::create_dir_all(&dest)?;

        let (kind, bytes) = if path.is_dir() {
            copy_dir_recursive(path, &dest)?;
            (EntryKind::Dir, size)
        } else {
            fs::copy(path, dest.join("entry"))?;
            (EntryKind::File, size)
        };

        let meta = SnapshotMeta {
            id,
            original: path.to_path_buf(),
            existed: true,
            kind,
            created_at: now_ms(),
            bytes,
        };
        self.write_meta(&meta)?;
        Ok(meta)
    }

    fn write_meta(&self, meta: &SnapshotMeta) -> Result<()> {
        let json = serde_json::to_string_pretty(meta)
            .map_err(|e| UndoError::BadMeta(e.to_string()))?;
        fs::write(self.meta_path(&meta.id), json)?;
        Ok(())
    }

    pub fn meta(&self, id: &str) -> Result<SnapshotMeta> {
        let p = self.meta_path(id);
        if !p.exists() {
            return Err(UndoError::SnapshotNotFound(id.to_string()));
        }
        let text = fs::read_to_string(p)?;
        serde_json::from_str(&text).map_err(|e| UndoError::BadMeta(e.to_string()))
    }

    /// 恢复快照。
    ///
    /// - `existed: false` → 删除目标路径（如果现在存在）
    /// - `existed: true`  → 用快照内容覆盖目标路径
    pub fn restore(&self, id: &str) -> Result<usize> {
        let meta = self.meta(id)?;
        let dest = self.data_dir(id);

        if !meta.existed {
            // 快照时不存在 → 撤销 = 删除现在的东西
            if meta.original.exists() {
                remove_any(&meta.original)?;
                return Ok(1);
            }
            return Ok(0);
        }

        // 先把现有目标删掉，避免类型冲突（文件↔目录）
        if meta.original.exists() {
            remove_any(&meta.original)?;
        }

        if let Some(parent) = meta.original.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        match meta.kind {
            EntryKind::File => {
                fs::copy(dest.join("entry"), &meta.original)?;
                Ok(1)
            }
            EntryKind::Dir => {
                copy_dir_recursive(&dest, &meta.original)?;
                Ok(1)
            }
            EntryKind::Symlink | EntryKind::Missing => Ok(0),
        }
    }

    /// 丢弃快照，释放空间。
    pub fn discard(&self, id: &str) -> Result<()> {
        let dir = self.data_dir(id);
        if dir.exists() {
            remove_any(&dir)?;
        }
        let meta = self.meta_path(id);
        if meta.exists() {
            fs::remove_file(meta)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- 文件工具

fn entry_size(path: &Path) -> Result<u64> {
    if path.is_file() {
        Ok(fs::metadata(path)?.len())
    } else if path.is_dir() {
        let mut total = 0;
        for e in fs::read_dir(path)? {
            let e = e?;
            total += entry_size(&e.path())?;
        }
        Ok(total)
    } else {
        Ok(0)
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn remove_any(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- 撤销算法

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UndoReport {
    pub units_undone: usize,
    pub steps_reverted: usize,
    pub files_restored: usize,
    /// 无法自动撤销的效果，需要人工处理
    pub skipped: Vec<String>,
    pub elapsed_ms: u64,
}

/// 回退到某个工作单元「之前」——即撤销该单元及其之后的所有步骤。
///
/// 判定依据是**步骤序号**而不是 unit 归属：没有挂 unit 的步骤也必须能被撤销，
/// 否则 agent 只要不建 unit，撤销功能就形同虚设。
///
/// `target_unit_id` 为 `None` 时撤销全部。
pub fn undo_to_unit(
    store: &Store,
    snaps: &SnapshotStore,
    task_id: &str,
    target_unit_id: Option<&str>,
) -> Result<UndoReport> {
    let started = now_ms();
    let mut report = UndoReport::default();

    let all_steps = store
        .list_steps(task_id)
        .map_err(|e| UndoError::Store(e.to_string()))?;
    let units = store
        .list_units(task_id)
        .map_err(|e| UndoError::Store(e.to_string()))?;

    // 决定从哪个步骤序号开始撤销
    let from_index: u32 = match target_unit_id {
        None => 0,
        Some(uid) => {
            if !units.iter().any(|u| u.id == uid) {
                return Err(UndoError::Store(format!("工作单元不存在：{uid}")));
            }
            match all_steps.iter().find(|s| s.unit_id.as_deref() == Some(uid)) {
                Some(s) => s.index,
                None => {
                    // 该 unit 没有步骤：用创建时间定位到其后第一个步骤
                    let u = units.iter().find(|u| u.id == uid).unwrap();
                    all_steps
                        .iter()
                        .find(|s| s.started_at >= u.created_at)
                        .map(|s| s.index)
                        .unwrap_or(all_steps.len() as u32)
                }
            }
        }
    };

    let to_undo: Vec<_> = all_steps.iter().filter(|s| s.index >= from_index).collect();

    // 逆序处理副作用
    for step in to_undo.iter().rev() {
        match &step.effect {
            Some(taskd_core::Effect::SnapshotRef { snapshot, .. }) => match snaps.restore(snapshot) {
                Ok(n) => {
                    report.files_restored += n;
                    report.steps_reverted += 1;
                }
                Err(e) => report
                    .skipped
                    .push(format!("步骤 {}：快照恢复失败（{e}）", step.index)),
            },
            Some(taskd_core::Effect::InverseOp { op, .. }) => {
                report
                    .skipped
                    .push(format!("步骤 {}：逆操作 `{op}` 需要工具自行处理", step.index));
            }
            Some(taskd_core::Effect::Compensable { compensation, .. }) => {
                report.skipped.push(format!(
                    "步骤 {}：需执行补偿动作 `{compensation}`",
                    step.index
                ));
            }
            Some(taskd_core::Effect::Irreversible { reason }) => {
                report
                    .skipped
                    .push(format!("步骤 {}：不可逆（{reason}）", step.index));
            }
            None => report.steps_reverted += 1,
        }
    }

    // 标记受影响的工作单元：目标单元及其之后、且尚未被放弃的
    let target_pos = match target_unit_id {
        Some(uid) => units.iter().position(|u| u.id == uid),
        None => None,
    };
    let affected: Vec<&taskd_core::WorkUnit> = match (target_unit_id, target_pos) {
        (Some(_), Some(pos)) => units[pos..]
            .iter()
            .filter(|u| u.outcome != taskd_core::Outcome::Abandoned)
            .collect(),
        (Some(_), None) => {
            return Err(UndoError::Store(format!(
                "工作单元不存在：{}",
                target_unit_id.unwrap_or_default()
            )))
        }
        (None, _) => units
            .iter()
            .filter(|u| u.outcome != taskd_core::Outcome::Abandoned)
            .collect(),
    };

    for u in &affected {
        store
            .finish_unit(&u.id, taskd_core::Outcome::Abandoned)
            .map_err(|e| UndoError::Store(e.to_string()))?;
    }
    report.units_undone = affected.len();

    report.elapsed_ms = (now_ms() - started).max(0) as u64;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use taskd_core::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "taskd-undo-test-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn capture_and_restore_file() {
        let root = tmp_root("file");
        let snaps = SnapshotStore::new(root.join("snaps")).unwrap();
        let target = root.join("a.txt");
        fs::write(&target, "原始内容").unwrap();

        let meta = snaps.capture(&target).unwrap();
        assert!(meta.existed);
        assert_eq!(meta.kind, EntryKind::File);

        fs::write(&target, "被改坏了").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "被改坏了");

        let n = snaps.restore(&meta.id).unwrap();
        assert_eq!(n, 1);
        assert_eq!(fs::read_to_string(&target).unwrap(), "原始内容");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn capture_missing_then_restore_deletes_created_file() {
        let root = tmp_root("missing");
        let snaps = SnapshotStore::new(root.join("snaps")).unwrap();
        let target = root.join("new.txt");

        // 快照时不存在
        let meta = snaps.capture(&target).unwrap();
        assert!(!meta.existed);
        assert_eq!(meta.kind, EntryKind::Missing);

        // 现在创建了它
        fs::write(&target, "新建的").unwrap();
        assert!(target.exists());

        // 撤销 → 应该被删掉
        let n = snaps.restore(&meta.id).unwrap();
        assert_eq!(n, 1);
        assert!(!target.exists());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn restore_directory_tree() {
        let root = tmp_root("dir");
        let snaps = SnapshotStore::new(root.join("snaps")).unwrap();
        let dir = root.join("proj");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "A").unwrap();
        fs::write(dir.join("sub/b.txt"), "B").unwrap();

        let meta = snaps.capture(&dir).unwrap();
        assert_eq!(meta.kind, EntryKind::Dir);

        fs::write(dir.join("a.txt"), "A-mod").unwrap();
        fs::remove_file(dir.join("sub/b.txt")).unwrap();
        fs::write(dir.join("c.txt"), "C").unwrap();

        snaps.restore(&meta.id).unwrap();

        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "A");
        assert_eq!(fs::read_to_string(dir.join("sub/b.txt")).unwrap(), "B");
        assert!(!dir.join("c.txt").exists());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn size_limit_is_enforced() {
        let root = tmp_root("limit");
        let snaps = SnapshotStore::new(root.join("snaps"))
            .unwrap()
            .with_limit(10);
        let target = root.join("big.bin");
        fs::write(&target, vec![0u8; 64]).unwrap();

        let err = snaps.capture(&target).unwrap_err();
        assert!(matches!(err, UndoError::TooLarge { .. }));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discard_frees_space() {
        let root = tmp_root("discard");
        let snaps = SnapshotStore::new(root.join("snaps")).unwrap();
        let target = root.join("a.txt");
        fs::write(&target, "x").unwrap();
        let meta = snaps.capture(&target).unwrap();

        snaps.discard(&meta.id).unwrap();
        assert!(!snaps.data_dir(&meta.id).exists());
        assert!(!snaps.meta_path(&meta.id).exists());

        fs::remove_dir_all(&root).ok();
    }

    // ---------------------------------------------------------- 撤销算法

    fn setup_task(store: &Store, goal: &str) -> Task {
        let mut t = Task::new(goal);
        store.create_task(&t).unwrap();
        t.transition_to(TaskState::Running).unwrap();
        store.update_task_state(&t.id, TaskState::Running).unwrap();
        t
    }

    #[test]
    fn undo_unit_restores_files() {
        let root = tmp_root("alg");
        let docs = root.join("docs");
        fs::create_dir_all(&docs).unwrap();
        let file = docs.join("note.txt");
        fs::write(&file, "v1").unwrap();

        let store = Store::open_in_memory().unwrap();
        let snaps = SnapshotStore::new(root.join("snaps")).unwrap();
        let task = setup_task(&store, "改文件");

        // 单元 1：改文件
        let unit = WorkUnit::new(&task.id, WorkUnitKind::Attempt);
        store.create_unit(&unit).unwrap();

        let meta = snaps.capture(&file).unwrap();
        fs::write(&file, "v2").unwrap();

        let mut step = Step::new(&task.id, 0, StepKind::ToolCall, "写入 v2").with_unit(&unit.id);
        step = step.with_effect(Effect::SnapshotRef {
            root: docs.display().to_string(),
            snapshot: meta.id.clone(),
        });
        step.finish(StepResult::Ok { payload: None });
        store.append_step(&step).unwrap();
        store.finish_unit(&unit.id, Outcome::Succeeded).unwrap();

        // 撤销
        let report = undo_to_unit(&store, &snaps, &task.id, Some(&unit.id)).unwrap();
        assert_eq!(report.units_undone, 1);
        assert_eq!(report.files_restored, 1);
        assert!(report.skipped.is_empty());
        assert_eq!(fs::read_to_string(&file).unwrap(), "v1");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undo_reports_irreversible_effects() {
        let root = tmp_root("irrev");
        let store = Store::open_in_memory().unwrap();
        let snaps = SnapshotStore::new(root.join("snaps")).unwrap();
        let task = setup_task(&store, "发邮件");

        let unit = WorkUnit::new(&task.id, WorkUnitKind::Change);
        store.create_unit(&unit).unwrap();

        let mut step = Step::new(&task.id, 0, StepKind::ToolCall, "发送通知").with_unit(&unit.id);
        step = step.with_effect(Effect::Irreversible {
            reason: "邮件已发出".into(),
        });
        step.finish(StepResult::Ok { payload: None });
        store.append_step(&step).unwrap();
        store.finish_unit(&unit.id, Outcome::Succeeded).unwrap();

        let report = undo_to_unit(&store, &snaps, &task.id, Some(&unit.id)).unwrap();
        assert_eq!(report.units_undone, 1);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].contains("不可逆"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undo_is_idempotent_on_units() {
        let root = tmp_root("idem");
        let store = Store::open_in_memory().unwrap();
        let snaps = SnapshotStore::new(root.join("snaps")).unwrap();
        let task = setup_task(&store, "空任务");

        let unit = WorkUnit::new(&task.id, WorkUnitKind::Attempt);
        store.create_unit(&unit).unwrap();
        store.finish_unit(&unit.id, Outcome::Succeeded).unwrap();

        let r1 = undo_to_unit(&store, &snaps, &task.id, Some(&unit.id)).unwrap();
        assert_eq!(r1.units_undone, 1);

        // 第二次撤销：单元已 Abandoned，不应再计数（幂等）
        let r2 = undo_to_unit(&store, &snaps, &task.id, Some(&unit.id)).unwrap();
        assert_eq!(r2.units_undone, 0, "已撤销的单元不应重复计数");

        fs::remove_dir_all(&root).ok();
    }
}
