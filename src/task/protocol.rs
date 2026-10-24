use serde_json::{json, Value};
use std::path::Path;

fn task_system_prompt(ws: &Path) -> String {
    format!(
        r#"你是 Windows 上的编程助手。工作区：{ws}

工具调用（逐行写，禁止 JSON）：
  TOOL: read
  PATH: 文件或目录路径

  TOOL: search
  TEXT: 关键词
  PATH: 搜索目录（可选）

  TOOL: run
  CMD: PowerShell 命令
  PATH: 执行目录（可选）

示例：
用户：看看 main.rs
助手：TOOL: read
PATH: src/main.rs

用户：找 connect 函数
助手：TOOL: search
TEXT: connect

用户：Python 版本
助手：TOOL: run
CMD: python --version

规则：
- 先调工具获取信息，拿到结果后再决定下一步。
- 路径仅限工作区内。
- 任务完成后直接输出答案。
- 连续两次工具调用无有效信息则告知用户。
"#,
        ws = ws.display()
    )
}

/// OpenAI / llama.cpp 兼容的工具定义
pub fn tool_definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "read",
                "description": "读取工作区内的文件内容（带行号），或列出目录内容。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "工作区内的文件或目录路径，默认工作区根目录"
                        }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search",
                "description": "在工作区内递归搜索文本，返回匹配的文件、行号和内容。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "搜索起始目录，默认工作区根目录"
                        },
                        "text": {
                            "type": "string",
                            "description": "要查找的文本（不区分大小写）"
                        }
                    },
                    "required": ["text"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run",
                "description": "在工作区内执行 PowerShell 命令并返回输出与退出码。执行前需要用户审批。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "cmd": {
                            "type": "string",
                            "description": "要执行的 PowerShell 命令"
                        },
                        "path": {
                            "type": "string",
                            "description": "可选：命令执行目录，默认工作区根目录"
                        }
                    },
                    "required": ["cmd"]
                }
            }
        }
    ])
}

pub fn make_system_message(ws: &Path) -> Value {
    json!({
        "role": "system",
        "content": task_system_prompt(ws),
    })
}

/// 从模型输出解析工具调用。格式：
///   TOOL: read
///   PATH: xxx
/// 或
///   TOOL: search
///   PATH: xxx
///   TEXT: xxx
/// 或
///   TOOL: run
///   CMD: xxx
pub fn parse_tool(text: &str) -> Option<(String, Value)> {
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.len() < 5 {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if !lower.starts_with("tool:") {
            continue;
        }
        let after = trimmed[5..].trim();
        let name_lower = after.split_whitespace().next().unwrap_or("");
        if !matches!(name_lower, "read" | "search" | "run") {
            continue;
        }
        let name = name_lower.to_string();

        let mut args = serde_json::Map::new();
        for rest in text.lines().skip(i + 1) {
            let rtrim = rest.trim();
            if rtrim.is_empty() {
                continue;
            }
            let rlower = rtrim.to_lowercase();
            if rlower.starts_with("tool:") {
                break;
            }
            if let Some(colon) = rtrim.find(':') {
                let key = rtrim[..colon].trim().to_uppercase();
                let val = rtrim[colon + 1..].trim();
                if matches!(key.as_str(), "PATH" | "TEXT" | "CMD") && !val.is_empty() {
                    args.insert(key.to_lowercase(), Value::String(val.to_string()));
                }
            }
        }
        if matches!(name.as_str(), "search") && !args.contains_key("text") {
            continue;
        }
        if matches!(name.as_str(), "run") && !args.contains_key("cmd") {
            continue;
        }
        return Some((name, Value::Object(args)));
    }
    None
}

/// GBNF 语法：阻止模型以 `{` 开头输出 JSON（文本兜底路径用）
pub const TOOL_GRAMMAR: &str = r#"root ::= [^{}]*"#;

/// 清理模型输出中的 TOOL: 块
pub fn strip_tool_block(text: &str) -> String {
    if parse_tool(text).is_none() {
        return text.trim().to_string();
    }
    let mut result = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if lower.starts_with("tool:") {
            break;
        }
        let upper = trimmed.to_uppercase();
        if upper.starts_with("PATH:") || upper.starts_with("TEXT:") || upper.starts_with("CMD:") {
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result.trim().to_string()
}