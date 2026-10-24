use futures_channel::mpsc::UnboundedSender;
use portable_pty::{Child, PtySize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

// ── 全局会话注册表 ──────────────────────────────────────────────

pub(super) static NEXT_ID: AtomicU32 = AtomicU32::new(1);
pub(super) static INITED: AtomicBool = AtomicBool::new(false);
pub(super) static SESSIONS: LazyLock<Mutex<HashMap<u32, SessionHandle>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(super) static ACTIVE_ID: AtomicU32 = AtomicU32::new(0);
pub(super) static JS_CMD_TX: LazyLock<Mutex<Option<UnboundedSender<serde_json::Value>>>> =
    LazyLock::new(|| Mutex::new(None));

pub(super) struct SessionHandle {
    #[allow(dead_code)]
    pub name: String,
    pub cmd_tx: UnboundedSender<SessionCmd>,
    /// PSReadLine 初始化完成后置 true，在此之前 handle_js_message 丢弃所有输入
    pub ready: Arc<AtomicBool>,
    pub child: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
}

pub(super) enum SessionCmd {
    Write(Vec<u8>),
    Resize(PtySize),
}

/// 向 JS 发送一条消息（经由全局命令通道，由多路复用任务转发给 eval）
pub(super) fn send_to_js(msg: serde_json::Value) {
    if let Some(tx) = JS_CMD_TX.lock().unwrap().as_ref() {
        let _ = tx.unbounded_send(msg);
    }
}

/// 供菜单等外部组件向当前活跃终端注入命令。返回是否成功。
pub fn inject_shell_command(cmd: String) -> bool {
    let sessions = SESSIONS.lock().unwrap();
    let id = ACTIVE_ID.load(Ordering::SeqCst);
    let sid = if id != 0 && sessions.contains_key(&id) {
        id
    } else {
        sessions.keys().next().copied().unwrap_or(0)
    };
    if sid == 0 {
        return false;
    }
    if let Some(s) = sessions.get(&sid) {
        let _ = s
            .cmd_tx
            .unbounded_send(SessionCmd::Write(cmd.into_bytes()));
        true
    } else {
        false
    }
}

/// 切换终端配色（暗/亮）
pub fn set_theme(dark: bool) {
    send_to_js(serde_json::json!({ "t": "theme", "dark": dark }));
}

/// 设置终端字体大小
pub fn set_font_size(size: u32) {
    send_to_js(serde_json::json!({ "t": "fontSize", "size": size }));
}

pub(super) fn create_session() -> (u32, String) {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let name = format!(">_ {id}");
    (id, name)
}

pub(super) fn set_active(id: u32) {
    ACTIVE_ID.store(id, Ordering::SeqCst);
}

pub(super) fn close_session(id: u32) {
    let handle = SESSIONS.lock().unwrap().remove(&id);
    if let Some(h) = handle {
        if let Ok(mut guard) = h.child.lock() {
            if let Some(c) = guard.as_mut() {
                let _ = c.kill();
            }
            *guard = None;
        }
    }
    if ACTIVE_ID.load(Ordering::SeqCst) == id {
        let next = SESSIONS.lock().unwrap().keys().next().copied().unwrap_or(0);
        ACTIVE_ID.store(next, Ordering::SeqCst);
    }
}

/// 处理 JS 发来的消息（输入/resize）
pub(super) fn handle_js_message(msg: serde_json::Value) {
    let id = msg.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    match msg.get("t").and_then(|v| v.as_str()) {
        Some("in") => {
            if let Some(data) = msg.get("data").and_then(|v| v.as_str()) {
                let sessions = SESSIONS.lock().unwrap();
                if let Some(s) = sessions.get(&id) {
                    // PSReadLine 初始化完成前丢弃所有输入，避免 SetConsoleMode 刷缓冲区
                    if s.ready.load(Ordering::SeqCst) {
                        let _ = s
                            .cmd_tx
                            .unbounded_send(SessionCmd::Write(data.as_bytes().to_vec()));
                    }
                }
            }
        }
        Some("rs") => {
            let cols = msg.get("cols").and_then(|v| v.as_u64()).unwrap_or(100) as u16;
            let rows = msg.get("rows").and_then(|v| v.as_u64()).unwrap_or(30) as u16;
            let sessions = SESSIONS.lock().unwrap();
            if let Some(s) = sessions.get(&id) {
                let _ = s.cmd_tx.unbounded_send(SessionCmd::Resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                }));
            }
        }
        _ => {}
    }
}