use serde_json::Value;

use super::shell::psh_run;
use super::state::workspace_path;

pub fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// 拒绝明显无效的命令（模型误解了 run 的语义）
fn is_nonsense_cmd(cmd: &str) -> bool {
    let lower = cmd.trim().to_lowercase();
    lower.is_empty()
        || lower == "powershell"
        || lower == "powershell.exe"
        || lower == "cmd"
        || lower == "cmd.exe"
        || lower == "bash"
        || lower == "run"
        // 模型把模板 <命令> 字面输出
        || lower.starts_with('<') && lower.ends_with('>')
}

pub fn execute_tool_blocking(name: &str, args: &Value) -> Result<String, String> {
    match name {
        "run" => {
            let command = arg_str(args, "cmd").ok_or("缺少 CMD")?;
            if is_nonsense_cmd(&command) {
                return Err(format!(
                    "无效命令：\"{command}\"。run 后面要写具体操作，例如 run dir 或 run type 文件名。"
                ));
            }
            let cwd = workspace_path().display().to_string();
            let (output, code) = psh_run(&command, Some(cwd.as_str()))?;
            match code {
                Some(0) | None => Ok(output),
                Some(c) => Err(format!("{output}\n[退出码 {c}]")),
            }
        }
        other => Err(format!("未知工具：{other}")),
    }
}