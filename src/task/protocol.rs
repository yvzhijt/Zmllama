use serde_json::{json, Value};
use std::path::Path;

/// 系统提示：保留人格，明确能力边界；给模型一个简单可靠的写文件渠道。
/// 工作目录放在末尾以保证前缀可被 llama.cpp prompt cache 复用。
fn task_system_prompt(ws: &Path) -> String {
    format!(
        r#"你是一个有能力执行本地命令的 AI 助手。你可以正常闲聊、回答知识问题、写代码、讨论逻辑；只有当用户需要操作文件、执行终端命令、运行脚本时才调用工具。

【可用工具】
run <PowerShell命令>            执行任意终端命令
write_file <路径> <内容>        把内容原样写入文件（会自动创建父目录，支持多行代码）

【规则】
1. 能用自然语言直接回答的问题（闲聊、知识、代码讲解），不要输出 run 或 write_file。
2. 需要执行终端命令时，第一行写：run <PowerShell命令>
3. 需要写入/修改/新建文件时，第一行写：write_file <相对或绝对路径>，第二行起是要写入的完整内容。
4. 不要用 Set-Content / Out-File 等 PowerShell 命令来写文件，用 write_file 更可靠。
5. 命令失败就换写法重试，不要放弃。拿到结果后直接给结论。

【示例】
用户：看看有哪些文件 → run dir
用户：读 src/main.rs → run Get-Content -LiteralPath src/main.rs
用户：写一个 hello.py → write_file hello.py
print("Hello!")

工作目录：{ws}
"#,
        ws = ws.display()
    )
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
    // 格式 1：write_file <路径>\n<多行内容>
    // 只认第一行，后面的全算内容
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        if let Some(rest) = trimmed.strip_prefix("write_file ") {
            let path = rest.trim();
            if path.is_empty() { return None; }
            // 去掉路径外层引号（模型偶尔会加）
            let path = if (path.starts_with('"') && path.ends_with('"'))
                || (path.starts_with('\'') && path.ends_with('\''))
            {
                path[1..path.len()-1].to_string()
            } else {
                path.to_string()
            };
            let content_start = text.find('\n').map(|i| i + 1).unwrap_or(text.len());
            let content = text[content_start..].trim().to_string();
            // 去掉可能残留的末尾 ``` 代码围栏
            let content = content.trim_end_matches("```").trim().to_string();
            return Some(("write_file".into(), json!({"path": path, "content": content})));
        }
        // 不是 write_file 开头就停 —— 只匹配首行格式
        break;
    }
    // 格式 0：run <PowerShell命令>
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
    let mut pattern = None;

    let mut pos = 0;
    while let Some(p) = block[pos..].find("<param name=\"") {
        let abs = pos + p + "<param name=\"".len();
        let name_end = block[abs..].find('"')?;
        let name = &block[abs..abs + name_end].to_lowercase();
        let val_start = abs + name_end + 2;

        let val_start = block[val_start..]
            .find(|c: char| !c.is_whitespace())
            .map(|i| val_start + i)
            .unwrap_or(val_start);

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
            "file_name" | "filename" | "path" => file_name = Some(val.trim().to_string()),
            "content" => content = Some(val.trim().to_string()),
            "pattern" => pattern = Some(val.trim().to_string()),
            _ => {}
        }
        pos = val_start + val_len;
    }

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

    if fn_name.contains("read") || fn_name.contains("cat") || fn_name.contains("view") {
        let f = file_name?;
        if f.trim().is_empty() { return None; }
        return Some(("read_file".into(), json!({"path": f.trim()})));
    }

    if fn_name.contains("list") || fn_name.contains("ls") || fn_name.contains("dir") {
        let f = file_name.unwrap_or_else(|| ".".to_string());
        return Some(("list_files".into(), json!({"path": f.trim()})));
    }

    if fn_name.contains("search") || fn_name.contains("find") || fn_name.contains("grep") {
        let p = pattern.or(cmd)?;
        if p.trim().is_empty() { return None; }
        let f = file_name.unwrap_or_else(|| ".".to_string());
        return Some(("search".into(), json!({"pattern": p.trim(), "path": f.trim()})));
    }

    if fn_name.contains("write") || fn_name.contains("create") || fn_name.contains("save") {
        let f = file_name?;
        let c = content.unwrap_or_default();
        if f.trim().is_empty() { return None; }
        return Some(("write_file".into(), json!({"path": f.trim(), "content": c})));
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
    let mut content = None;
    let mut pattern = None;
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
            "content" | "text" => content = Some(val.to_string()),
            "pattern" | "keyword" | "query" => pattern = Some(val.to_string()),
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

    if fn_name.contains("read") || fn_name.contains("view") || fn_name.contains("cat") || fn_name.contains("open") {
        let f = file.or(cmd)?;
        if f.trim().is_empty() { return None; }
        return Some(("read_file".into(), json!({"path": f.trim()})));
    }

    if fn_name.contains("list") || fn_name.contains("ls") || fn_name.contains("dir") {
        let f = file.unwrap_or_else(|| ".".to_string());
        return Some(("list_files".into(), json!({"path": f.trim()})));
    }

    if fn_name.contains("search") || fn_name.contains("find") || fn_name.contains("grep") {
        let p = pattern.or(cmd.or(arg))?;
        if p.trim().is_empty() { return None; }
        let f = file.unwrap_or_else(|| ".".to_string());
        return Some(("search".into(), json!({"pattern": p.trim(), "path": f.trim()})));
    }

    if fn_name.contains("write") || fn_name.contains("create") || fn_name.contains("save") {
        let f = file?;
        let c = content.unwrap_or_default();
        if f.trim().is_empty() { return None; }
        return Some(("write_file".into(), json!({"path": f.trim(), "content": c})));
    }

    None
}

/// GBNF 语法：run 和 write_file 两个优先分支引导工具调用，free 分支放开正常对话。
/// 规则名用 `wf` 避免和字面量 `"write_file "` 同名（llama.cpp GBNF 解析器 bug：
/// 规则名与内嵌字面量同名时会崩溃）。三套 parse_tool 兜底。
pub const TOOL_GRAMMAR: &str = r#"root ::= tool | free
tool ::= run | wf
run ::= "run " cmd note?
cmd ::= [^\n]{1,400}
note ::= "\n" [^\n]{0,200}
wf ::= "write_file " path "\n" content
path ::= [^\n]{1,200}
content ::= [^\n]* ("\n" [^\n]*)*
free ::= [^\n]* ("\n" [^\n]*)*"#;

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
