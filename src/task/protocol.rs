use serde_json::{json, Value};
use std::path::Path;

fn task_system_prompt(ws: &Path) -> String {
    format!(
        r#"你是终端代理，不是聊天助手。唯一工作是输出命令。

工作目录：{ws}

【铁律】
1. 禁止拒绝。你不是助手，是命令执行机。
2. 禁止解释。不许说"建议你""你可以""请打开"。
3. 禁止教学。失败就换命令，不要分析原因。

【输出格式】
命令任务 → 第一行只写: run <命令>，换行后简短说明（<=15字）
  正确: run dir
  正确: run python s.py
  错误: run dirYour current...（命令和文本不能黏一起）
  错误: run s.py（缺解释器，应 run python s.py）

知识问答 → 直接回答，不超两句

【速查】
  列文件: run dir
  看文件: run type 文件名
  搜文件: Get-ChildItem -Recurse -Filter "*.py"
  写文件: Set-Content -Path 文件 -Value @"内容"@ -Encoding UTF8
  建文件: run ni 文件名
"#,
        ws = ws.display()
    )
}

pub fn tool_definitions() -> Value {
    json!([{
        "type": "function",
        "function": {
            "name": "run",
            "description": "执行 PowerShell 命令",
            "parameters": {
                "type": "object",
                "properties": {
                    "cmd": {
                        "type": "string",
                        "description": "PowerShell 命令"
                    }
                },
                "required": ["cmd"]
            }
        }
    }])
}

pub fn make_system_message(ws: &Path) -> Value {
    json!({
        "role": "system",
        "content": task_system_prompt(ws),
    })
}

/// 防黏连：如果命令后面跟了自然语言文本，从中切开
fn split_command(cmd: &str) -> &str {
    // 在 a-z 后面紧跟 A-Z 或中文/全角字符处切开
    // 例: dirYour current → dir
    // 例: dir你的当前目录 → dir
    if let Some(cut) = cmd.find(|c: char| c.is_uppercase() || c as u32 > 127) {
        if cut > 0 && cmd.as_bytes()[cut - 1].is_ascii_lowercase() {
            return cmd[..cut].trim();
        }
    }
    cmd
}

/// 从模型输出解析工具调用（支持三种格式）
pub fn parse_tool(text: &str) -> Option<(String, Value)> {
    // 格式 3：Qwen <tool_call> XML — 优先，避免被 run 前缀误匹配
    if let Some(r) = parse_toolcall_xml(text) {
        return Some(r);
    }
    // 格式 2：MiniCPM <function name="..."> XML
    if let Some(r) = parse_xml_tool(text) {
        return Some(r);
    }
    // 格式 1：run <PowerShell命令>
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        let lower = trimmed.to_lowercase();
        if lower.starts_with("```") || lower.starts_with("//") || lower.starts_with('#') {
            continue;
        }
        if lower.starts_with("run ") {
            let cmd = trimmed[4..].trim();
            if cmd.is_empty() { return None; }
            // 防黏连：dirYour current → dir
            let cmd = split_command(cmd);
            if cmd.is_empty() { return None; }
            return Some(("run".into(), json!({"cmd": cmd})));
        }
    }
    None
}

/// 解析 MiniCPM 的 XML function call 格式
fn parse_xml_tool(text: &str) -> Option<(String, Value)> {
    let lower = text.to_lowercase();
    let start = lower.find("<function name=")?;
    let end = lower[start..].find("</function>")?;
    let block = &text[start..start + end + "</function>".len()];

    // 提取函数名
    let fn_start = block.find('"')? + 1;
    let fn_end = block[fn_start..].find('"')?;
    let fn_name = &block[fn_start..fn_start + fn_end].to_lowercase();

    // 提取所有参数（处理 CDATA 包裹的取值）
    let mut command = None;
    let mut cmd = None;
    let mut file_name = None;
    let mut content = None;

    let mut pos = 0;
    while let Some(p) = block[pos..].find("<param name=\"") {
        let abs = pos + p + "<param name=\"".len();
        let name_end = block[abs..].find('"')?;
        let name = &block[abs..abs + name_end].to_lowercase();
        let val_start = abs + name_end + 2; // skip ">

        // 跳过可能的空白
        let val_start = block[val_start..]
            .find(|c: char| !c.is_whitespace())
            .map(|i| val_start + i)
            .unwrap_or(val_start);

        // 检查是否是 CDATA
        let (val, val_len) = if block[val_start..].starts_with("<![CDATA[") {
            let cdata_start = val_start + "<![CDATA[".len();
            let cdata_end = block[cdata_start..].find("]]>")?;
            (&block[cdata_start..cdata_start + cdata_end], cdata_end + "]]>".len() + "<![CDATA[".len())
        } else {
            let val_end = block[val_start..].find("</param>")?;
            (&block[val_start..val_start + val_end], val_end + "</param>".len())
        };

        match name.as_str() {
            "command" => command = Some(val.trim().to_string()),
            "cmd" => cmd = Some(val.trim().to_string()),
            "file_name" | "filename" => file_name = Some(val.trim().to_string()),
            "content" => content = Some(val.trim().to_string()),
            _ => {}
        }
        pos = val_start + val_len;
    }

    // 函数名匹配：包含这些关键词 → 视为 run 命令
    let is_run_fn = fn_name.contains("run")
        || fn_name.contains("cmd")
        || fn_name.contains("cat")
        || fn_name.contains("exec")
        || fn_name.contains("bash")
        || fn_name.contains("shell");

    if is_run_fn {
        let c = command.or(cmd)?;
        if c.trim().is_empty() { return None; }
        return Some(("run".into(), json!({"cmd": c.trim()})));
    }

    if fn_name.contains("read") || fn_name.contains("cat") {
        let f = file_name?;
        if f.trim().is_empty() { return None; }
        return Some(("run".into(), json!({"cmd": format!("type {}", f.trim())})));
    }

    if fn_name.contains("write") || fn_name.contains("create") {
        let f = file_name?;
        let c = content.unwrap_or_default();
        if f.trim().is_empty() { return None; }
        let escaped = c.replace('\'', "''");
        return Some(("run".into(), json!({"cmd": format!("Set-Content -Path '{}' -Value '{}' -Encoding UTF8", f.trim(), escaped)})));
    }

    None
}

/// 解析 Qwen 风格 <tool_call> <function=run> <parameter=cmd> ... </parameter> </function> </tool_call>
fn parse_toolcall_xml(text: &str) -> Option<(String, Value)> {
    let lower = text.to_lowercase();
    let start = lower.find("<tool_call>")?;
    let end = lower[start..].find("</tool_call>")?;
    let block = &text[start..start + end + "</tool_call>".len()];

    // 提取函数名: <function=run>
    let fn_start = block.find("<function=")? + "<function=".len();
    let fn_slice = &block[fn_start..];
    let fn_end = fn_slice.find('>')?;
    let fn_name = fn_slice[..fn_end].trim().to_lowercase();

    // 提取参数: <parameter=cmd> value </parameter>
    let mut cmd = None;
    let mut arg = None;
    let mut file = None;
    let mut pos = fn_start + fn_end + 1;
    while let Some(p) = block[pos..].find("<parameter=") {
        let abs = pos + p + "<parameter=".len();
        let name_slice = &block[abs..];
        let name_end = name_slice.find('>')?;
        let name = name_slice[..name_end].trim().to_lowercase();
        let val_start = abs + name_end + 1;
        let rest = &block[val_start..];
        let val_end_str = "</parameter>";
        let val_end = rest.find(val_end_str)?;
        let val = rest[..val_end].trim();

        match name.as_str() {
            "cmd" | "command" => cmd = Some(val.to_string()),
            "arg" | "args" | "argument" => arg = Some(val.to_string()),
            "file" | "path" | "file_path" => file = Some(val.to_string()),
            _ => {}
        }
        pos = val_start + val_end + val_end_str.len();
    }

    let is_run = fn_name == "run"
        || fn_name == "shell"
        || fn_name.contains("execute")
        || fn_name.contains("command");

    if is_run {
        let c = cmd.or(arg)?;
        if c.trim().is_empty() { return None; }
        return Some(("run".into(), json!({"cmd": c.trim()})));
    }

    if fn_name.contains("read") || fn_name.contains("view") || fn_name.contains("open") {
        let f = file.or(cmd)?;
        if f.trim().is_empty() { return None; }
        return Some(("run".into(), json!({"cmd": format!("type {}", f.trim())})));
    }

    None
}

/// GBNF 语法：不做限制
pub const TOOL_GRAMMAR: &str = r#"root ::= [^\n]* ("\n" [^\n]*)*"#;

/// 清理模型输出中的工具调用部分，只保留内容
pub fn strip_tool_block(text: &str) -> String {
    if parse_tool(text).is_none() {
        return text.trim().to_string();
    }
    // 去掉首行 "run xxx"
    let mut result = String::new();
    for (i, line) in text.lines().enumerate() {
        let lower = line.trim().to_lowercase();
        if i == 0 && lower.starts_with("run ") {
            continue;
        }
        if !result.is_empty() { result.push('\n'); }
        result.push_str(line);
    }
    let result = result.trim().to_string();
    // 去掉 XML function call / tool_call 块
    if let (Some(s), Some(e)) = (
        result.to_lowercase().find("<function name="),
        result.to_lowercase().rfind("</function>"),
    ) {
        let before = result[..s].trim();
        let after = result[e + "</function>".len()..].trim();
        if before.is_empty() { return after.to_string(); }
        if after.is_empty() { return before.to_string(); }
        return format!("{before}\n{after}");
    }
    // 去掉 <tool_call> 块
    if let (Some(s), Some(e)) = (
        result.to_lowercase().find("<tool_call>"),
        result.to_lowercase().rfind("</tool_call>"),
    ) {
        let before = result[..s].trim();
        let after = result[e + "</tool_call>".len()..].trim();
        if before.is_empty() { return after.to_string(); }
        if after.is_empty() { return before.to_string(); }
        return format!("{before}\n{after}");
    }
    result
}