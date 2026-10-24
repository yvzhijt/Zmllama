use dioxus::prelude::*;
use serde_json::{json, Value};
use std::path::Path;

use super::agent::run_agent;
use super::llm::reset_tools_supported;
use super::protocol::make_system_message;
use super::state::{cancel_task, respond_approval, ApprovalInfo, Entry, ToolState};

// ── UI 组件 ───────────────────────────────────────────────────

#[component]
pub fn TaskContent(workspace: Signal<String>) -> Element {
    let mut entries = use_signal(Vec::<Entry>::new);
    let mut history = use_signal(Vec::<Value>::new);
    let mut input = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut status = use_signal(|| "空闲".to_string());
    let mut pending = use_signal::<Option<ApprovalInfo>>(|| None);
    let mut enter_sent = use_signal(|| false);

    use_effect(move || {
        let ws = workspace();
        let mut h = history.write();
        if let Some(first) = h.first_mut() {
            if first.get("role").and_then(|v| v.as_str()) == Some("system") {
                let msg = make_system_message(Path::new(&ws));
                first["content"] = msg["content"].clone();
            }
        }
    });

    let mut do_send = move || -> bool {
        let text = input.read().trim().to_string();
        if text.is_empty() || busy() {
            return false;
        }
        entries.write().push(Entry::User(text.clone()));
        {
            let mut h = history.write();
            if h.is_empty() {
                h.push(make_system_message(Path::new(&workspace())));
            }
            h.push(json!({ "role": "user", "content": text }));
        }
        input.set(String::new());
        busy.set(true);
        status.set("思考中".into());
        spawn(run_agent(entries, history, busy, status, pending));
        true
    };

    let on_send_click = move |_| {
        do_send();
    };

    let on_stop = move |_| {
        cancel_task();
    };

    let on_reset = move |_| {
        cancel_task();
        reset_tools_supported();
        entries.set(Vec::new());
        history.set(Vec::new());
        input.set(String::new());
        pending.set(None);
        if busy() {
            status.set("正在停止…".into());
        } else {
            status.set("空闲".into());
        }
    };

    let cur_input = input.read().clone();
    let is_busy = *busy.read();
    let cur_status = status.read().clone();
    let cur_pending = pending.read().clone();
    let list = entries.read().clone();
    let ws_display = workspace();

    rsx! {
        div { class: "task-info",
            span { class: "task-workspace", "工作区：{ws_display}" }
            span { class: "task-status", "{cur_status}" }
            button { class: "mini-btn", title: "清除历史，重新开始", onclick: on_reset, "⟲ 重置" }
        }

        div { class: "chat-messages",
            if list.is_empty() {
                div { class: "chat-empty",
                    div { class: "chat-empty-title", "任务模式" }
                    div { class: "chat-empty-hint",
                        "描述任务，我会调用文件/终端工具分步完成（写操作需审批）"
                    }
                }
            }
            for (i, entry) in list.iter().enumerate() {
                match entry {
                    Entry::User(text) => rsx! {
                        div { key: "{i}", class: "msg user",
                            span { class: "msg-role", "你" }
                            "{text}"
                        }
                    },
                    Entry::Thinking(text) => rsx! {
                        div { key: "{i}", class: "msg thinking",
                            details { open: true,
                                summary { class: "thinking-summary", "🤔 思考过程" }
                                pre { class: "thinking-content", "{text}" }
                            }
                        }
                    },
                    Entry::Assistant(text) => rsx! {
                        div { key: "{i}", class: "msg assistant",
                            span { class: "msg-role", "AI" }
                            "{text}"
                        }
                    },
                    Entry::Tool { name, args, state, output } => rsx! {
                        div { key: "{i}", class: "tool-card",
                            div { class: "tool-card-head",
                                span { class: "tool-name", "{name}" }
                                span {
                                    class: match state {
                                        ToolState::Running => "tool-state running",
                                        ToolState::Ok => "tool-state ok",
                                        ToolState::Err => "tool-state err",
                                        ToolState::Denied => "tool-state denied",
                                    },
                                    match state {
                                        ToolState::Running => "执行中…",
                                        ToolState::Ok => "成功",
                                        ToolState::Err => "失败",
                                        ToolState::Denied => "已拒绝",
                                    }
                                }
                            }
                            if !args.is_empty() && args != "{}" {
                                div { class: "tool-args", "{args}" }
                            }
                            if !output.is_empty() {
                                pre { class: "tool-output", "{output}" }
                            }
                        }
                    },
                }
            }
            if let Some(req) = cur_pending {
                div { class: "approval-card",
                    div { class: "approval-title", "需要审批：{req.tool}" }
                    pre { class: "approval-detail", "{req.detail}" }
                    div { class: "approval-btns",
                        button {
                            class: "btn-allow",
                            onclick: move |_| respond_approval(req.id, true),
                            "允许"
                        }
                        button {
                            class: "btn-deny",
                            onclick: move |_| respond_approval(req.id, false),
                            "拒绝"
                        }
                    }
                }
            }
        }

        div {
            class: "chat-input-area",
            style: "flex-direction: row; align-items: flex-end; gap: 8px;",
            textarea {
                class: "chat-input",
                style: "flex: 1; min-height: 42px; max-height: 120px; resize: none;",
                placeholder: "描述任务… (Enter 发送，Shift+Enter 换行)",
                value: "{cur_input}",
                oninput: move |e| {
                    if enter_sent() {
                        enter_sent.set(false);
                        input.set(String::new());
                    } else {
                        input.set(e.value());
                    }
                },
                onkeydown: move |evt| {
                    if evt.key() == Key::Enter && !evt.modifiers().contains(Modifiers::SHIFT) {
                        evt.stop_propagation();
                        if do_send() {
                            enter_sent.set(true);
                        }
                    }
                },
            }
            button {
                class: "chat-send",
                onclick: on_send_click,
                disabled: is_busy,
                title: "发送 (Enter)",
                "→"
            }
            button {
                class: "chat-stop",
                onclick: on_stop,
                disabled: !is_busy,
                title: "停止",
                "■"
            }
        }
    }
}