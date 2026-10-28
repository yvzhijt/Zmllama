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

pub(super) fn dbg(_msg: &str) {}

pub(super) struct SessionHandle {
    #[allow(dead_code)]
    pub name: String,
    pub cmd_tx: UnboundedSender<SessionCmd>,
    /// PSReadLine 初始化完成后�?true
    pub ready: Arc<AtomicBool>,
    pub child: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
    pub pending_in: Arc<Mutex<Vec<Vec<u8>>>>,
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

/// Agent 输出透明写入终端（灰色 ANSI 前缀 [Agent]）
#[allow(dead_code)]
pub fn write_agent_output(line: &str) {
    let data = format!("\x1b[90m[Agent]\x1b[0m {}\r\n", line);
    send_to_js(serde_json::json!({ "t": "agent_out", "data": data }));
}

/// Agent 命令标题（写入终端，灰色分隔线形式）
#[allow(dead_code)]
pub fn write_agent_cmd(cmd: &str, args: &str) {
    let data = format!("\x1b[90m──── ⚡ {} {} ────\x1b[0m\r\n", cmd, args);
    send_to_js(serde_json::json!({ "t": "agent_out", "data": data }));
}

/// 发送 Agent 完成状态行到终端
#[allow(dead_code)]
pub fn write_agent_done(ok: bool, name: &str) {
    let (color, mark) = if ok {
        ("90", "✓")
    } else {
        ("91", "✗")
    };
    let data = format!("\x1b[{color}m[Agent {mark}] {name}\x1b[0m\r\n", color = color, mark = mark, name = name);
    send_to_js(serde_json::json!({ "t": "agent_out", "data": data }));
}

/// 切换终端配色（暗/亮）
pub fn set_theme(dark: bool) {
    send_to_js(serde_json::json!({ "t": "theme", "dark": dark }));
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

/// 过滤 xterm.js 内部 CSI 响应，避免回灌 PTY 干扰 PSReadLine
fn is_csi_response(data: &str) -> bool {
    let b = data.as_bytes();
    if b.is_empty() || b[0] != 0x1b {
        return false;
    }
    // FocusIn / FocusOut
    if data == "\x1b[I" || data == "\x1b[O" {
        return true;
    }
    // Cursor Position Report（CSI ... R），如 \x1b[1;1R
    if b.len() >= 3 && b[1] == b'[' && b.last() == Some(&b'R') {
        return true;
    }
    false
}

/// 处理 JS 发来的消息（输入/resize）
pub(super) fn handle_js_message(msg: serde_json::Value) {
    let id = msg.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    match msg.get("t").and_then(|v| v.as_str()) {
        Some("in") => {
            if let Some(data) = msg.get("data").and_then(|v| v.as_str()) {
                // 过滤 xterm.js 内部 CSI 响应（CPR、FocusIn/Out），
                // 避免回灌 PTY 导致 PSReadLine 吞掉第一个输入字符
                if is_csi_response(data) {
                    dbg(&format!("in id={id} data={data:?} FILTERED_CSI"));
                    return;
                }
                let sessions = SESSIONS.lock().unwrap();
                if let Some(s) = sessions.get(&id) {
                    let bytes = data.as_bytes().to_vec();
                    let ready = s.ready.load(Ordering::SeqCst);
                    dbg(&format!("in id={id} data={data:?} ready={ready} pending_count={}", 
                        s.pending_in.lock().map(|g| g.len()).unwrap_or(0)));
                    if ready {
                        let _ = s.cmd_tx.unbounded_send(SessionCmd::Write(bytes));
                    } else {
                        if let Ok(mut guard) = s.pending_in.lock() {
                            guard.push(bytes);
                        }
                    }
                } else {
                    dbg(&format!("in id={id} data={data:?} SESSION_NOT_FOUND"));
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
        Some("dbg") => {
            if let Some(m) = msg.get("msg").and_then(|v| v.as_str()) {
                dbg(m);
            }
        }
        _ => {}
    }
}