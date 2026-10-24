use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

// ── 全局状态 ───────────────────────────────────────────────────

/// 任务模式工作区（默认 = exe 所在目录，可由「文件 → 打开文件夹」更改）
static WORKSPACE: LazyLock<Mutex<PathBuf>> = LazyLock::new(|| {
    Mutex::new(
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from(".")),
    )
});

pub fn set_workspace(path: PathBuf) {
    *WORKSPACE.lock().unwrap() = path;
}

pub fn workspace_path() -> PathBuf {
    WORKSPACE.lock().unwrap().clone()
}

/// 任务取消标志
pub static TASK_CANCEL: AtomicBool = AtomicBool::new(false);

/// 共享 HTTP 客户端（带超时，避免卡死）
pub static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .pool_max_idle_per_host(4)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// 等待审批的工具调用：id → 应答通道
pub static APPROVALS: LazyLock<Mutex<HashMap<u64, futures_channel::oneshot::Sender<bool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub static NEXT_APPROVAL: AtomicU64 = AtomicU64::new(1);

pub fn cancel_task() {
    TASK_CANCEL.store(true, Ordering::SeqCst);
    let mut map = APPROVALS.lock().unwrap();
    for (_, tx) in map.drain() {
        let _ = tx.send(false);
    }
}

/// 供 UI 审批按钮调用
pub fn respond_approval(id: u64, allow: bool) {
    if let Some(tx) = APPROVALS.lock().unwrap().remove(&id) {
        let _ = tx.send(allow);
    }
}

// ── 数据模型 ───────────────────────────────────────────────────

#[derive(Clone, PartialEq)]
pub enum ToolState {
    Running,
    Ok,
    Err,
    Denied,
}

#[derive(Clone, PartialEq)]
pub enum Entry {
    User(String),
    Assistant(String),
    Thinking(String),
    Tool {
        name: String,
        args: String,
        state: ToolState,
        output: String,
    },
}

#[derive(Clone, PartialEq)]
pub struct ApprovalInfo {
    pub id: u64,
    pub tool: String,
    pub detail: String,
}