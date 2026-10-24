use dioxus::prelude::*;
use futures_channel::mpsc::unbounded;
use futures_util::{select, FutureExt, StreamExt};
use std::sync::atomic::Ordering;

use super::js::build_init_script;
use super::pty::start_session;
use super::state::{self, create_session, handle_js_message, send_to_js, set_active};

// ── 组件 ───────────────────────────────────────────────────────

fn do_switch_tab(mut active: Signal<u32>, tab_id: u32) {
    active.set(tab_id);
    set_active(tab_id);
    send_to_js(serde_json::json!({ "t": "activate", "id": tab_id }));
}

fn do_close_tab(
    mut active: Signal<u32>,
    mut sessions: Signal<Vec<(u32, String)>>,
    tab_id: u32,
) {
    if sessions.read().len() <= 1 {
        return;
    }
    state::close_session(tab_id);
    sessions.write().retain(|(id, _)| *id != tab_id);
    send_to_js(serde_json::json!({ "t": "dispose", "id": tab_id }));
    let remain = sessions.read().first().map(|(id, _)| *id).unwrap_or(0);
    if remain != 0 {
        active.set(remain);
        set_active(remain);
        send_to_js(serde_json::json!({ "t": "activate", "id": remain }));
    }
}

#[component]
pub fn TerminalPanel(width_pct: f64, dark_theme: bool) -> Element {
    let mut sessions = use_signal(Vec::<(u32, String)>::new);
    let mut active = use_signal(|| 0u32);

    // 初始化：单 ev 通道，双向复用（Rust→JS 命令/输出，JS→Rust 输入/resize）
    // 防重入：swap 保证无论 effect 并发跑几次，初始化只执行一次
    use_effect(move || {
        if state::INITED.swap(true, Ordering::SeqCst) {
            return;
        }
        let (cmd_tx, mut cmd_rx) = unbounded::<serde_json::Value>();
        *state::JS_CMD_TX.lock().unwrap() = Some(cmd_tx);

        let mut ev = document::eval(&build_init_script());
        spawn(async move {
            let (id0, name0) = create_session();
            let _ = ev.send(serde_json::json!({ "t": "create", "id": id0 }));
            let prompt_ready = start_session(id0);
            // 等待 PowerShell 提示符就绪后再激活终端，
            // 避免 PSReadLine 初始化时 SetConsoleMode 刷掉 ConPTY 输入缓冲
            let _ = prompt_ready.await;
            let _ = ev.send(serde_json::json!({ "t": "activate", "id": id0 }));
            // 启动时把当前主题推给终端（JS 默认暗色）
            let _ = ev.send(serde_json::json!({ "t": "theme", "dark": dark_theme }));
            set_active(id0);
            sessions.write().push((id0, name0));
            active.set(id0);

            // 双路 select：公平调度输出和输入，避免输出洪泛阻塞输入
            loop {
                enum Route {
                    ToJs(serde_json::Value),
                    FromJs(serde_json::Value),
                    End,
                }
                let route;
                {
                    let recv_fut = ev.recv::<serde_json::Value>().fuse();
                    futures_util::pin_mut!(recv_fut);
                    route = select! {
                        m = cmd_rx.next() => match m {
                            Some(msg) => Route::ToJs(msg),
                            None => Route::End,
                        },
                        r = recv_fut => match r {
                            Ok(msg) => Route::FromJs(msg),
                            Err(_) => Route::End,
                        },
                    };
                }
                match route {
                    Route::ToJs(m) => {
                        if ev.send(m).is_err() {
                            break;
                        }
                    }
                    Route::FromJs(m) => handle_js_message(m),
                    Route::End => break,
                }
            }
        });
    });

    let tabs = sessions.read().clone();
    let cur_active = *active.read();
    let sessions_count = tabs.len();

    rsx! {
        div {
            class: "terminal-panel",
            style: "flex: 0 0 {width_pct}%;",
            div { class: "tab-bar",
                for (tid, tname) in tabs.into_iter() {
                    div {
                        key: "{tid}",
                        class: format!("tab-item{}", if tid == cur_active { " active" } else { "" }),
                        onclick: move |_| do_switch_tab(active, tid),
                        span { class: "tab-label", "{tname}" }
                        if sessions_count > 1 {
                            button {
                                class: "tab-close",
                                onclick: move |e| {
                                    e.stop_propagation();
                                    do_close_tab(active, sessions, tid);
                                },
                                "×"
                            }
                        }
                    }
                }
                button {
                    class: "tab-add",
                    onclick: move |_| {
                        let (id, name) = create_session();
                        send_to_js(serde_json::json!({ "t": "create", "id": id }));
                        let ready = start_session(id);
                        // 等提示符就绪后再 focus，避免 PSReadLine 初始化刷掉输入
                        spawn(async move {
                            let _ = ready.await;
                            send_to_js(serde_json::json!({ "t": "activate", "id": id }));
                        });
                        set_active(id);
                        sessions.write().push((id, name));
                        active.set(id);
                    },
                    "+"
                }
            }
            div { class: "terminal-body", id: "terminal" }
        }
    }
}