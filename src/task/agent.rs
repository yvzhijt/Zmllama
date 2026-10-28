use dioxus::prelude::*;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

use super::llm::{push_tool_result, stream_turn};
use super::state::{
    workspace_path, ApprovalInfo, Entry, ToolState, APPROVALS, NEXT_APPROVAL, TASK_CANCEL,
};
use super::tools::{arg_str, execute_tool_blocking};
use super::truncate_for_context;
use super::truncate_output;

// ── 工具执行编排 ───────────────────────────────────────────────

async fn execute_tool(name: String, args: Value) -> Result<String, String> {
    super::run_blocking(move || execute_tool_blocking(&name, &args)).await
}

fn tool_needs_approval(name: &str, args: &Value) -> bool {
    match name {
        "run" => {
            let cmd = arg_str(args, "cmd").unwrap_or_default().to_lowercase();
            DANGEROUS_PATTERNS.iter().any(|p| cmd.contains(p))
        }
        "write_file" => true,
        _ => false,
    }
}

/// 危险命令关键词（匹配到则需审批）
const DANGEROUS_PATTERNS: &[&str] = &[
    "format", "del /s", "del /q", "rmdir /s", "rmdir /q",
    "remove-item -recurse", "rm -rf", "rm -r",
    ":\\windows", ":\\system32", ":\\program files",
    "cargo clean", "git push --force",
    // 写文件类：模型通过 run 落盘时会命中，先让用户确认
    "set-content", "out-file", "new-item",
];

fn approval_detail(name: &str, args: &Value) -> String {
    match name {
        "run" => format!(
            "命令：{}\n目录：{}",
            arg_str(args, "cmd").unwrap_or_default(),
            workspace_path().display()
        ),
        "write_file" => format!(
            "写入文件：{}\n内容：{}",
            arg_str(args, "path").unwrap_or_default(),
            arg_str(args, "content").unwrap_or_default()
        ),
        _ => args.to_string(),
    }
}

async fn request_approval(
    pending: &mut Signal<Option<ApprovalInfo>>,
    tool: &str,
    detail: String,
) -> bool {
    let id = NEXT_APPROVAL.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = futures_channel::oneshot::channel();
    APPROVALS.lock().unwrap().insert(id, tx);
    pending.set(Some(ApprovalInfo {
        id,
        tool: tool.to_string(),
        detail,
    }));
    let allow = rx.await.unwrap_or(false);
    pending.set(None);
    allow
}

// ── 上下文预算 ────────────────────────────────────────────────
// 4B 模型 + 4096 上下文下，留给对话的预算压到 2600 token（其余留给
// 模板、特殊 token 与单步 320 token 生成）。超出就丢最早的整组消息。

const CONTEXT_BUDGET: usize = 2600;
/// 单步最大生成量：放宽后允许模型输出较长回答或代码片段。
const MAX_TOKENS_PER_STEP: u32 = 600;

/// 粗略 token 估算：CJK 约 1 token/字，ASCII 约 1 token/4 字符。
fn estimate_tokens(s: &str) -> usize {
    let mut cjk = 0usize;
    let mut ascii = 0usize;
    for c in s.chars() {
        if (c as u32) > 0x7f {
            cjk += 1;
        } else {
            ascii += 1;
        }
    }
    cjk + ascii / 4
}

fn msg_tokens(m: &Value) -> usize {
    let mut n = 4;
    if let Some(s) = m.get("content").and_then(|v| v.as_str()) {
        n += estimate_tokens(s);
    }
    if let Some(arr) = m.get("tool_calls").and_then(|v| v.as_array()) {
        for c in arr {
            n += estimate_tokens(&c.to_string());
        }
    }
    n
}

/// 从最旧的对话消息开始裁剪，保留 system 与最近的消息。
/// 若被删除的 assistant 带 tool_calls，紧随其后的 tool 结果一并删除，
/// 否则 llama.cpp 会因为孤立的 tool 消息报错。
fn trim_history(history: &mut Vec<Value>) {
    let mut total: usize = history.iter().map(msg_tokens).sum();
    while total > CONTEXT_BUDGET && history.len() > 2 {
        let removed = history.remove(1);
        total = total.saturating_sub(msg_tokens(&removed));
        while history
            .get(1)
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str())
            == Some("tool")
        {
            let orphan = history.remove(1);
            total = total.saturating_sub(msg_tokens(&orphan));
        }
    }
}

// ── Agent 主循环 ──────────────────────────────────────────────

pub async fn run_agent(
    mut entries: Signal<Vec<Entry>>,
    mut history: Signal<Vec<Value>>,
    mut busy: Signal<bool>,
    mut status: Signal<String>,
    mut pending: Signal<Option<ApprovalInfo>>,
) {
    TASK_CANCEL.store(false, Ordering::SeqCst);
    busy.set(true);

    // 小模型在 5 步之后基本开始随机游走，超出即中止
    const MAX_STEPS: usize = 5;
    let mut last_tool_signature = String::new();
    let mut repeat_count = 0usize;

    for step in 0..MAX_STEPS {
        if TASK_CANCEL.load(Ordering::SeqCst) {
            status.set("已停止".into());
            break;
        }

        entries.write().push(Entry::Thinking(String::new()));
        let think_idx = entries.read().len() - 1;
        entries.write().push(Entry::Assistant(String::new()));
        let idx = entries.read().len() - 1;
        status.set(format!("思考中（第 {} 步）", step + 1));

        let mut msgs = history.read().clone();
        trim_history(&mut msgs);
        let result = {
            let mut on_delta = |d: &str| {
                entries.with_mut(|e| {
                    if let Some(Entry::Assistant(t)) = e.get_mut(idx) {
                        t.push_str(d);
                    }
                });
            };
            stream_turn(&msgs, MAX_TOKENS_PER_STEP, &mut on_delta).await
        };

        let turn = match result {
            Ok(t) => t,
            Err(e) => {
                entries.with_mut(|es| {
                    if let Some(Entry::Assistant(t)) = es.get_mut(idx) {
                        *t = format!("⚠ {e}");
                    }
                });
                status.set("出错".into());
                break;
            }
        };

        if TASK_CANCEL.load(Ordering::SeqCst) {
            entries.with_mut(|es| {
                if let Some(Entry::Assistant(t)) = es.get_mut(idx) {
                    *t = turn.content.clone();
                }
            });
            status.set("已停止".into());
            break;
        }

        if !turn.reasoning.trim().is_empty() {
            entries.with_mut(|es| {
                if let Some(Entry::Thinking(t)) = es.get_mut(think_idx) {
                    *t = turn.reasoning.trim().to_string();
                }
            });
        } else {
            entries.with_mut(|es| {
                if es.len() > think_idx && matches!(es[think_idx], Entry::Thinking(_)) {
                    es.remove(think_idx);
                }
            });
        }

        let display = if turn.legacy {
            super::protocol::strip_tool_block(&turn.content)
        } else {
            turn.content.trim().to_string()
        };
        let has_calls = !turn.tool_calls.is_empty();

        if display.is_empty() && has_calls {
            entries.with_mut(|es| {
                if idx < es.len() && matches!(es.get(idx), Some(Entry::Assistant(_))) {
                    es.remove(idx);
                }
            });
        } else {
            entries.with_mut(|es| {
                if let Some(Entry::Assistant(t)) = es.get_mut(idx) {
                    *t = display.clone();
                }
            });
        }

        if !has_calls {
            let lower = turn.content.to_lowercase();

            // 模型输出纯 "run" / "runxxx" 无命令体 → 格式纠正
            let missed_tool = lower.contains("run") && super::protocol::parse_tool(&turn.content).is_none()
                && turn.content.trim().chars().count() < 20;

            // 模型明确拒绝执行 —— 只匹配最明确的拒绝句式，避免误杀正常对话
            let refused = lower.contains("i can't execute")
                || lower.contains("i cannot execute")
                || lower.contains("i'm sorry")
                || lower.contains("i am sorry")
                || (lower.contains("我无法") && lower.contains("执行"))
                || (lower.contains("我不能") && lower.contains("执行"))
                // 幻觉出 PowerShell 提示符（这不是正常对话内容）
                || (lower.contains(":\\") && (lower.contains(">") || lower.contains("$")));

            if refused {
                history.write().push(json!({ "role": "assistant", "content": turn.content }));
                history.write().push(json!({
                    "role": "user",
                    "content": "不要拒绝，不要教学。失败了就换命令重试。回复格式：run <命令>"
                }));
                entries.with_mut(|es| {
                    if let Some(Entry::Assistant(t)) = es.get_mut(idx) {
                        *t = format!("⚠ 拒绝/教学，已纠正：{}", turn.content.trim());
                    }
                });
                status.set(format!("纠正拒绝（第 {} 步）", step + 1));
                continue;
            }

            if missed_tool {
                history.write().push(json!({ "role": "assistant", "content": turn.content }));
                history.write().push(json!({
                    "role": "user",
                    "content": "格式不对。请在回复第一行写：run <PowerShell命令>"
                }));
                entries.with_mut(|es| {
                    if let Some(Entry::Assistant(t)) = es.get_mut(idx) {
                        *t = format!("⚠ 格式错误，已重试：{}", turn.content.trim());
                    }
                });
                status.set(format!("纠正格式（第 {} 步）", step + 1));
                continue;
            }

            history.write().push(json!({ "role": "assistant", "content": turn.content }));
            status.set("完成".into());
            break;
        }

        let mut calls = turn.tool_calls;
        for (i, c) in calls.iter_mut().enumerate() {
            if c.id.is_empty() {
                c.id = format!("call_{step}_{i}");
            }
        }

        let tool_calls_json: Vec<Value> = calls
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "type": "function",
                    "function": {
                        "name": c.name,
                        "arguments": if c.arguments.trim().is_empty() { "{}" } else { c.arguments.as_str() }
                    }
                })
            })
            .collect();
        let content_value = if display.is_empty() {
            Value::Null
        } else {
            Value::String(display.clone())
        };
        history.write().push(json!({
            "role": "assistant",
            "content": content_value,
            "tool_calls": tool_calls_json,
        }));

        let mut stop = false;
        for (ci, call) in calls.iter().enumerate() {
            if TASK_CANCEL.load(Ordering::SeqCst) {
                for rest in &calls[ci..] {
                    push_tool_result(
                        &mut history,
                        &rest.id,
                        &rest.name,
                        "已取消：用户中断".into(),
                    );
                }
                status.set("已停止".into());
                stop = true;
                break;
            }

            let parsed = if call.arguments.trim().is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_str::<Value>(&call.arguments)
            };
            let args = match parsed {
                Ok(v @ Value::Object(_)) => v,
                Ok(_) => {
                    let msg = "工具参数必须是 JSON 对象".to_string();
                    entries.write().push(Entry::Tool {
                        name: call.name.clone(),
                        args: call.arguments.clone(),
                        state: ToolState::Err,
                        output: msg.clone(),
                    });
                    push_tool_result(&mut history, &call.id, &call.name, msg);
                    continue;
                }
                Err(e) => {
                    let msg = format!("参数解析失败：{e}");
                    entries.write().push(Entry::Tool {
                        name: call.name.clone(),
                        args: call.arguments.clone(),
                        state: ToolState::Err,
                        output: msg.clone(),
                    });
                    push_tool_result(&mut history, &call.id, &call.name, msg);
                    continue;
                }
            };

            let signature = format!("{}:{}", call.name, args);
            if signature == last_tool_signature {
                repeat_count += 1;
            } else {
                repeat_count = 0;
                last_tool_signature = signature;
            }

            entries.write().push(Entry::Tool {
                name: call.name.clone(),
                args: serde_json::to_string(&args).unwrap_or_default(),
                state: ToolState::Running,
                output: String::new(),
            });
            let tidx = entries.read().len() - 1;

            if repeat_count >= 2 {
                entries.with_mut(|es| {
                    if let Some(Entry::Tool { state, output, .. }) = es.get_mut(tidx) {
                        *state = ToolState::Err;
                        *output = "检测到重复的工具调用，已中止。".to_string();
                    }
                });
                push_tool_result(
                    &mut history,
                    &call.id,
                    &call.name,
                    "检测到重复的工具调用，已中止。请直接给出最终答复。".into(),
                );
                for rest in &calls[ci + 1..] {
                    push_tool_result(
                        &mut history,
                        &rest.id,
                        &rest.name,
                        "未执行：任务已中止".into(),
                    );
                }
                status.set("已中止（重复调用）".into());
                stop = true;
                break;
            }

            if tool_needs_approval(&call.name, &args) {
                status.set("等待审批".into());
                let allow = request_approval(
                    &mut pending,
                    &call.name,
                    approval_detail(&call.name, &args),
                )
                .await;
                if !allow {
                    entries.with_mut(|es| {
                        if let Some(Entry::Tool { state, output, .. }) = es.get_mut(tidx) {
                            *state = ToolState::Denied;
                            *output = "用户拒绝执行该操作。".to_string();
                        }
                    });
                    push_tool_result(
                        &mut history,
                        &call.id,
                        &call.name,
                        "用户拒绝执行该操作。请换一种方式，或直接给出结论。".into(),
                    );
                    if TASK_CANCEL.load(Ordering::SeqCst) {
                        for rest in &calls[ci + 1..] {
                            push_tool_result(
                                &mut history,
                                &rest.id,
                                &rest.name,
                                "已取消：用户中断".into(),
                            );
                        }
                        status.set("已停止".into());
                        stop = true;
                        break;
                    }
                    status.set("思考中".into());
                    continue;
                }
            }

            if TASK_CANCEL.load(Ordering::SeqCst) {
                for rest in &calls[ci..] {
                    push_tool_result(
                        &mut history,
                        &rest.id,
                        &rest.name,
                        "已取消：用户中断".into(),
                    );
                }
                status.set("已停止".into());
                stop = true;
                break;
            }

            status.set(format!("执行 {} …", call.name));

            let (live_tx, mut live_rx) = futures_channel::mpsc::unbounded::<String>();
            *super::shell::LIVE_TX.lock().unwrap() = Some(live_tx);
            let mut entries_live = entries;
            let tidx_live = tidx;
            spawn(async move {
                while let Some(line) = live_rx.next().await {
                    entries_live.with_mut(|es| {
                        if let Some(Entry::Tool {
                            state,
                            output: out,
                            ..
                        }) = es.get_mut(tidx_live)
                        {
                            if *state == ToolState::Running {
                                out.push_str(&line);
                                out.push('\n');
                            }
                        }
                    });
                }
            });

            let result = execute_tool(call.name.clone(), args).await;
            *super::shell::LIVE_TX.lock().unwrap() = None;

            let (state, output) = match result {
                Ok(o) => (ToolState::Ok, o),
                Err(e) => (ToolState::Err, format!("错误：{e}")),
            };

            let shown = truncate_output(&output);
            entries.with_mut(|es| {
                if let Some(Entry::Tool { state: s, output: out, .. }) = es.get_mut(tidx) {
                    *s = state;
                    *out = shown.clone();
                }
            });
            push_tool_result(
                &mut history,
                &call.id,
                &call.name,
                truncate_for_context(&output),
            );
        }

        if stop {
            break;
        }
    }

    busy.set(false);
    if status().is_empty() {
        status.set("空闲".into());
    }
}