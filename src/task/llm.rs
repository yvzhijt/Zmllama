use dioxus::prelude::*;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

use super::protocol::{parse_tool, tool_definitions, TOOL_GRAMMAR};
use super::state::{HTTP_CLIENT, TASK_CANCEL};
use super::{detect_model, LLM_BASE};

// ── 模型调用（SSE 流式 + 结构化工具调用）─────────────────────

#[derive(Default, Clone)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

pub struct TurnResult {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub legacy: bool,
    pub reasoning: String,
}

/// 服务端是否支持 tools 参数；2-4B 小模型默认关闭走文本协议
static TOOLS_SUPPORTED: AtomicBool = AtomicBool::new(false);

/// 重置 tools 支持标记（用于“重置”任务时恢复为关闭状态）
pub fn reset_tools_supported() {
    TOOLS_SUPPORTED.store(false, Ordering::SeqCst);
}

fn tool_rejected(status: reqwest::StatusCode, body: &str) -> bool {
    (status.is_client_error() || status.is_server_error())
        && body.to_lowercase().contains("tool")
}

#[derive(Default)]
struct StreamAccumulator {
    content: String,
    reasoning: String,
    accs: Vec<ToolCallAcc>,
}

impl StreamAccumulator {
    fn apply(&mut self, delta: &Value, on_delta: &mut dyn FnMut(&str)) {
        if let Some(s) = delta.get("content").and_then(|v| v.as_str()) {
            self.content.push_str(s);
            on_delta(s);
        }
        if let Some(s) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
            self.reasoning.push_str(s);
        }

        let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) else {
            return;
        };
        for c in calls {
            let idx = c.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            while self.accs.len() <= idx {
                self.accs.push(ToolCallAcc::default());
            }
            let acc = &mut self.accs[idx];

            if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    acc.id = id.to_string();
                }
            }
            if let Some(f) = c.get("function") {
                if let Some(name) = f.get("name").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        acc.name = name.to_string();
                    }
                }
                match f.get("arguments") {
                    Some(Value::String(s)) => acc.arguments.push_str(s),
                    Some(v) if !v.is_null() && acc.arguments.is_empty() => {
                        acc.arguments = v.to_string();
                    }
                    _ => {}
                }
            }
        }
    }

    fn finish(mut self) -> TurnResult {
        let mut tool_calls: Vec<ToolCall> = std::mem::take(&mut self.accs)
            .into_iter()
            .filter(|a| !a.name.is_empty())
            .map(|a| ToolCall {
                id: a.id,
                name: a.name,
                arguments: a.arguments,
            })
            .collect();

        let mut legacy = false;
        if tool_calls.is_empty() {
            if let Some((name, args)) = parse_tool(&self.content) {
                legacy = true;
                tool_calls.push(ToolCall {
                    id: String::new(),
                    name,
                    arguments: serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()),
                });
            }
        }

        TurnResult {
            content: self.content,
            tool_calls,
            legacy,
            reasoning: self.reasoning,
        }
    }
}

pub async fn stream_turn(
    messages: &[Value],
    mut on_delta: impl FnMut(&str),
) -> Result<TurnResult, String> {
    use tokio::time::{timeout, Duration};
    let model = detect_model().await?;
    let client = &HTTP_CLIENT;

    let mut use_tools = TOOLS_SUPPORTED.load(Ordering::SeqCst);
    let resp = loop {
        let mut body = json!({
            "model": &model,
            "messages": messages,
            "stream": true,
            // 采样参数针对 llama.cpp 中小量化模型调优：
            // - temperature 过低 + repeat_penalty 过低会让模型陷入循环/退化
            // - repeat_penalty 提高到 1.18 以抑制重复，代码生成更顺畅
            "temperature": 0.4,
            "top_p": 0.92,
            "top_k": 40,
            "max_tokens": 2048,
            "repeat_penalty": 1.18,
        });
        if use_tools {
            body["tools"] = tool_definitions();
            body["tool_choice"] = json!("auto");
        } else {
            body["grammar"] = json!(TOOL_GRAMMAR);
            body["response_format"] = json!({"type": "text"});
        }

        let req = client
            .post(format!("{}/chat/completions", LLM_BASE))
            .json(&body)
            .send();
        let resp_res = timeout(Duration::from_secs(180), req)
            .await
            .map_err(|_| "请求超时（180s）".to_string())?;
        let resp = resp_res.map_err(|e| format!("连接失败：{e}"))?;
        let status = resp.status();
        if status.is_success() {
            break resp;
        }
        let text = resp.text().await.unwrap_or_default();
        if use_tools && tool_rejected(status, &text) {
            TOOLS_SUPPORTED.store(false, Ordering::SeqCst);
            use_tools = false;
            continue;
        }
        return Err(format!("服务返回 {status}：{text}"));
    };

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut acc = StreamAccumulator::default();

    'stream: while let Some(chunk) = stream.next().await {
        if TASK_CANCEL.load(Ordering::SeqCst) {
            break;
        }
        let bytes = chunk.map_err(|e| format!("流读取错误：{e}"))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim_end_matches('\r').to_string();
            buf.drain(..=pos);
            let line = line.trim();
            if !line.starts_with("data:") {
                continue;
            }
            let payload = line[5..].trim();
            if payload == "[DONE]" {
                break 'stream;
            }
            let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            let Some(delta) = parsed.pointer("/choices/0/delta") else {
                continue;
            };
            acc.apply(delta, &mut on_delta);
        }
    }

    Ok(acc.finish())
}

pub fn push_tool_result(
    history: &mut Signal<Vec<Value>>,
    id: &str,
    name: &str,
    content: String,
) {
    history.write().push(json!({
        "role": "tool",
        "tool_call_id": id,
        "name": name,
        "content": content,
    }));
}