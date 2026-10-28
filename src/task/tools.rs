use base64::Engine;
use serde_json::Value;

use super::shell::psh_run;
use super::state::workspace_path;

pub fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn arg_nonempty(args: &Value, key: &str) -> Option<String> {
    arg_str(args, key).filter(|s| !s.trim().is_empty())
}

/// PowerShell 单引号字面量。路径里出现 ' 时不会把命令打断。
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
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
        || lower.starts_with('<') && lower.ends_with('>')
}

fn run_shell(cmd: &str) -> Result<String, String> {
    if is_nonsense_cmd(cmd) {
        return Err(format!(
            "无效命令：\"{cmd}\"。run 后面要写具体操作，例如 run dir 或 run type 文件名。"
        ));
    }
    let cwd = workspace_path().display().to_string();
    let (output, code) = psh_run(cmd, Some(cwd.as_str()))?;
    match code {
        Some(0) | None => Ok(output),
        Some(c) => Err(format!("{output}\n[退出码 {c}]")),
    }
}

pub fn execute_tool_blocking(name: &str, args: &Value) -> Result<String, String> {
    match name {
        "run" => {
            let command = arg_str(args, "cmd").ok_or("缺少 cmd 参数")?;
            run_shell(&command)
        }

        "read_file" => {
            let path = arg_nonempty(args, "path").ok_or("缺少 path 参数")?;
            run_shell(&format!(
                "Get-Content -LiteralPath {} -Raw -Encoding UTF8",
                ps_quote(&path)
            ))
        }

        "list_files" => {
            let path = arg_str(args, "path").unwrap_or_else(|| ".".to_string());
            run_shell(&format!("Get-ChildItem -LiteralPath {}", ps_quote(&path)))
        }

        "search" => {
            let pattern = arg_nonempty(args, "pattern").ok_or("缺少 pattern 参数")?;
            let dir = arg_str(args, "path").unwrap_or_else(|| ".".to_string());
            run_shell(&format!(
                "Get-ChildItem -LiteralPath {} -Recurse | Select-String -Pattern {} -SimpleMatch",
                ps_quote(&dir),
                ps_quote(&pattern)
            ))
        }

        "write_file" => {
            let path = arg_nonempty(args, "path").ok_or("缺少 path 参数")?;
            let content = arg_str(args, "content").unwrap_or_default();
            // 内容走 base64，避开 here-string / 引号 / 换行转义问题；直接写 UTF-8 字节
            let b64 = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
            run_shell(&format!(
                "$p={};$d=[IO.Path]::GetDirectoryName($p);if($d){{[IO.Directory]::CreateDirectory($d)|Out-Null}};[IO.File]::WriteAllBytes($p,[Convert]::FromBase64String('{b64}'))",
                ps_quote(&path)
            ))
        }

        other => Err(format!("未知工具：{other}")),
    }
}