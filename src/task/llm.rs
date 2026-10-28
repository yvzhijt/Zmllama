use dioxus::prelude::*;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

use super::protocol::{parse_tool, TOOL_GRAMMAR};
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
    max_tokens: u32,
    mut on_delta: impl FnMut(&str),
) -> Result<TurnResult, String> {
    use tokio::time::{timeout, Duration};
    let model = detect_model().await?;
    let client = &HTTP_CLIENT;

    // 采样参数：temperature 0.4 平衡对话自然度与工具选择稳定性；
    // grammar 保留 run 分支引导，free 分支放开正常回答。
    // cache_prompt 让固定 system 前缀命中 KV cache，省掉每步的 prefill。
    let body = json!({
        "model": &model,
        "messages": messages,
        "stream": true,
        "cache_prompt": true,
        "temperature": 0.4,
        "top_p": 0.9,
        "top_k": 50,
        "repeat_penalty": 1.12,
        "max_tokens": max_tokens,
        "grammar": TOOL_GRAMMAR,
        // 思维链模型（NeoHorse / Qwen3 等）默认把 token 全花在 reasoning_content
        // 上。显式关闭思考：实测单步从 ~10s 降到 ~2.5s。
        "chat_template_kwargs": { "enable_thinking": false },
    });

    let req = client
        .post(format!("{}/chat/completions", LLM_BASE))
        .json(&body)
        .send();
    let resp = timeout(Duration::from_secs(60), req)
        .await
        .map_err(|_| "连接超时（60s）".to_string())?
        .map_err(|e| format!("连接失败：{e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("服务返回 {status}：{text}"));
    }

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut acc = StreamAccumulator::default();

    'stream: loop {
        if TASK_CANCEL.load(Ordering::SeqCst) {
            break;
        }
        // 生成中断保护：20s 没有任何新数据就判定服务端卡死
        let next = match timeout(Duration::from_secs(20), stream.next()).await {
            Err(_) => return Err("生成超时（20s 无输出）".to_string()),
            Ok(None) => break,
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(e))) => return Err(format!("流读取错误：{e}")),
        };
        buf.push_str(&String::from_utf8_lossy(&next));

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