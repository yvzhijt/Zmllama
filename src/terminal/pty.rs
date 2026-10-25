use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_channel::oneshot;
use futures_util::StreamExt;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use super::state::{self, close_session, send_to_js, SessionCmd, SessionHandle};

// ── PTY 会话 ───────────────────────────────────────────────────

type PtyShell = (Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>);

fn spawn_shell() -> Result<PtyShell, String> {
    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty: {e}"))?;

    let portable_pty::PtyPair { slave, master } = pair;
    let home = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));

    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.arg("-NoLogo");
    cmd.arg("-NoProfile");
    cmd.arg("-NoExit");

    let prompt_def = "function prompt { $e=[char]27; $p=$pwd.Path|Split-Path -Leaf; \"$e[96m$p$e[0m $e[92m>$e[0m \" }";
    let encoded = {
        use base64::Engine;
        let bytes: Vec<u8> = prompt_def
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    };
    cmd.arg("-EncodedCommand");
    cmd.arg(encoded);

    let ws = crate::task::workspace_path();
    if ws.is_dir() {
        cmd.cwd(ws);
    } else if let Some(ref h) = home {
        cmd.cwd(h);
    }
    let child = match slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(_) => {
            let mut cmd = CommandBuilder::new_default_prog();
            if let Some(ref h) = home {
                cmd.cwd(h);
            }
            slave
                .spawn_command(cmd)
                .map_err(|e| format!("spawn shell: {e}"))?
        }
    };
    drop(slave);
    Ok((master, child))
}

/// 每读到数据立即发送（不批处理）；同时代答 CSI 6n
fn read_and_batch(
    reader: &mut dyn Read,
    tx: &UnboundedSender<String>,
    pty_tx: &UnboundedSender<SessionCmd>,
) {
    const PTY_QUERY: &[u8] = b"\x1b[6n";
    const PTY_REPLY: &[u8] = b"\x1b[1;1R";

    let mut buf = Vec::new();
    let mut leftover: Vec<u8> = Vec::new();
    let mut tail: Vec<u8> = Vec::new();

    loop {
        if !leftover.is_empty() {
            let s = String::from_utf8_lossy(&leftover).into_owned();
            leftover.clear();
            if tx.unbounded_send(s).is_err() {
                return;
            }
        }

        buf.resize(65536, 0);
        match reader.read(buf.as_mut_slice()) {
            Ok(0) => break,
            Ok(n) => {
                let data = buf[..n].to_vec();

                let mut combined = std::mem::take(&mut tail);
                combined.extend_from_slice(&data);
                if combined.windows(PTY_QUERY.len()).any(|w| w == PTY_QUERY) {
                    let _ =
                        pty_tx.unbounded_send(SessionCmd::Write(PTY_REPLY.to_vec()));
                }
                if combined.len() >= PTY_QUERY.len() {
                    tail.extend_from_slice(
                        &combined[combined.len() - (PTY_QUERY.len() - 1)..],
                    );
                }
                while tail.len() > PTY_QUERY.len() - 1 {
                    tail.drain(..1);
                }

                match std::str::from_utf8(&data) {
                    Ok(s) => {
                        if tx.unbounded_send(s.to_string()).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let valid = e.valid_up_to();
                        if valid > 0 {
                            let s =
                                String::from_utf8_lossy(&data[..valid]).into_owned();
                            if tx.unbounded_send(s).is_err() {
                                return;
                            }
                        }
                        if e.error_len().is_none() {
                            leftover.extend_from_slice(&data[valid..]);
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }
}

/// 启动一个新 PTY 会话（后台线程 + 注册）。
/// 返回 oneshot Receiver：PSReadLine 完全就绪后 fire。
pub(super) fn start_session(id: u32) -> oneshot::Receiver<()> {
    let (master, child) = match spawn_shell() {
        Ok((m, c)) => (m, c),
        Err(e) => {
            send_to_js(serde_json::json!({
                "t": "out", "id": id,
                "data": format!("\r\n\x1b[31m启动 shell 失败：{e}\x1b[0m\r\n")
            }));
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(());
            return rx;
        }
    };

    let mut reader = match master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            send_to_js(serde_json::json!({
                "t": "out", "id": id,
                "data": format!("\r\n\x1b[31m创建 reader 失败：{e}\x1b[0m\r\n")
            }));
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(());
            return rx;
        }
    };
    let writer = match master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            send_to_js(serde_json::json!({
                "t": "out", "id": id,
                "data": format!("\r\n\x1b[31m创建 writer 失败：{e}\x1b[0m\r\n")
            }));
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(());
            return rx;
        }
    };

    let (cmd_tx, mut cmd_rx) = unbounded::<SessionCmd>();
    let pty_tx = cmd_tx.clone();
    let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pending_in = Arc::new(std::sync::Mutex::new(Vec::new()));
    state::SESSIONS.lock().unwrap().insert(
        id,
        SessionHandle {
            name: format!(">_ {id}"),
            cmd_tx: cmd_tx.clone(),
            ready: Arc::clone(&ready),
            child: Arc::new(Mutex::new(Some(child))),
            pending_in: Arc::clone(&pending_in),
        },
    );
    state::ACTIVE_ID.store(id, Ordering::SeqCst);

    // PTY 读线程 → 批量解码 → 输出通道（并代答 CSI 6n）
    let (out_tx, mut out_rx) = unbounded::<String>();
    {
        let name = format!("pty-read-{id}");
        let _ = std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                read_and_batch(&mut reader, &out_tx, &pty_tx);
            });
    }

    // 第一批输出到达时：等待 PSReadLine 完成 SetConsoleMode 初始化，
    // 然后标记就绪、排空 pending 输入、fire oneshot 通知调用方。
    // 如果不等这 200ms 就直接发送 pending 输入，PSReadLine 会吞掉第一个字符。
    let (ready_tx, ready_rx) = oneshot::channel::<()>();

    let pending_in_out = Arc::clone(&pending_in);
    let cmd_tx_out = cmd_tx.clone();
    dioxus::prelude::spawn(async move {
        let mut ready_tx = Some(ready_tx);
        while let Some(chunk) = out_rx.next().await {
            if let Some(tx) = ready_tx.take() {
                state::dbg(&format!("pty id={id} FIRST_OUTPUT len={} preview={:?}", 
                    chunk.len(), chunk.chars().take(40).collect::<String>()));
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                ready.store(true, Ordering::SeqCst);
                if let Ok(mut guard) = pending_in_out.lock() {
                    let drained = guard.drain(..).collect::<Vec<_>>();
                    state::dbg(&format!("pty id={id} READY draining {} pending chunks", drained.len()));
                    for data in drained {
                        let _ = cmd_tx_out.unbounded_send(SessionCmd::Write(data));
                    }
                }
                state::dbg(&format!("pty id={id} READY done, firing oneshot"));
                let _ = tx.send(());
            }
            send_to_js(serde_json::json!({ "t": "out", "id": id, "data": chunk }));
        }
        send_to_js(serde_json::json!({ "t": "dead", "id": id }));
        close_session(id);
    });

    // 输入/resize → PTY
    let writer = Arc::new(Mutex::new(Some(writer)));
    dioxus::prelude::spawn(async move {
        while let Some(cmd) = cmd_rx.next().await {
            match cmd {
                SessionCmd::Write(data) => {
                    state::dbg(&format!("pty-write id={id} len={} data={:?}", 
                        data.len(), String::from_utf8_lossy(&data)));
                    if let Ok(mut guard) = writer.lock() {
                        if let Some(ref mut w) = *guard {
                            if w.write_all(&data).is_err() {
                                break;
                            }
                            let _ = w.flush();
                        }
                    }
                }
                SessionCmd::Resize(sz) => {
                    let _ = master.resize(sz);
                }
            }
        }
    });

    ready_rx
}