use serde_json::Value;
use std::path::{Path, PathBuf};

use super::shell::{psh_run, validate_command};
use super::state::workspace_path;
use super::truncate_output;

fn resolve_path(p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() {
        pb
    } else {
        workspace_path().join(pb)
    }
}

pub fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// 规范化路径并确认它位于工作区内（含符号链接解析后的校验）
fn ensure_in_workspace(path: &Path) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(workspace_path())
        .map_err(|e| format!("工作区不可访问：{e}"))?;
    // 尽量解析符号链接；若目标不存在也要做词法校验防止路径逃逸
    match std::fs::canonicalize(path) {
        Ok(target) => {
            if target.strip_prefix(&root).is_ok() {
                Ok(target)
            } else {
                Err(format!("路径超出工作区：{}", path.display()))
            }
        }
        Err(e) => {
            // 词法校验：将路径拼接到工作区后检查是否仍在工作区内
            let joined = if path.is_absolute() { path.to_path_buf() } else { workspace_path().join(path) };
            let mut norm = PathBuf::new();
            for comp in joined.components() {
                match comp {
                    std::path::Component::ParentDir => {
                        if norm == root {
                            return Err(format!("路径超出��作区：{}", path.display()));
                        }
                        norm.pop();
                    }
                    std::path::Component::CurDir => {}
                    std::path::Component::Normal(c) => norm.push(c),
                    std::path::Component::RootDir => norm.push(comp.as_os_str()),
                    std::path::Component::Prefix(p) => {
                        norm.push(p.as_os_str());
                    }
                }
            }
            if norm.strip_prefix(&root).is_err() && norm != root {
                // 允许直接等于工作区根目录
                if norm != root {
                    return Err(format!("路径超出工作区：{}", path.display()));
                }
            }
            Err(format!("无法访问 {}：{e}", path.display()))
        }
    }
}

/// 把工作区内的路径转成便于阅读的相对路径
fn workspace_relative(p: &Path) -> String {
    let Ok(root) = std::fs::canonicalize(workspace_path()) else {
        return p.display().to_string();
    };
    match p.strip_prefix(&root) {
        Ok(r) if r.as_os_str().is_empty() => ".".to_string(),
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => p.display().to_string(),
    }
}

pub fn execute_tool_blocking(name: &str, args: &Value) -> Result<String, String> {
    match name {
        "read" => {
            let p =
                arg_str(args, "path").unwrap_or_else(|| workspace_path().display().to_string());
            let path = ensure_in_workspace(&resolve_path(&p))?;
            let shown_path = workspace_relative(&path);
            let meta = std::fs::metadata(&path)
                .map_err(|e| format!("无法访问 {}：{e}", path.display()))?;
            if meta.is_dir() {
                let mut items: Vec<(String, bool)> = Vec::new();
                for entry in std::fs::read_dir(&path)
                    .map_err(|e| format!("无法列出 {}：{e}", path.display()))?
                    .flatten()
                {
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    items.push((entry.file_name().to_string_lossy().into_owned(), is_dir));
                }
                items.sort_by(|a, b| a.0.cmp(&b.0));
                let total = items.len();
                let mut out = format!("{shown_path}（{total} 项）\n");
                for (n, is_dir) in items.into_iter().take(300) {
                    out.push_str(if is_dir { "  [DIR]  " } else { "         " });
                    out.push_str(&n);
                    out.push('\n');
                }
                if total > 300 {
                    out.push_str(&format!("…（其余 {} 项省略）\n", total - 300));
                }
                Ok(out)
            } else {
                if meta.len() > 4 * 1024 * 1024 {
                    return Err("文件过大（>4MB），请缩小范围".into());
                }
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| format!("不是文本文件或读取失败：{e}"))?;
                let lines: Vec<&str> = content.lines().collect();
                let total = lines.len();
                let max_show = 200usize;
                let mut numbered = String::new();
                for (i, line) in lines.iter().take(max_show).enumerate() {
                    numbered.push_str(&format!("{:>5} | {}\n", i + 1, line));
                }
                let mut out = format!("{shown_path}（共 {total} 行）\n{}", numbered);
                if total > max_show {
                    out.push_str(&format!(
                        "…（其余 {} 行省略，共 {total} 行）\n",
                        total - max_show
                    ));
                }
                Ok(out)
            }
        }
        "search" => {
            let needle = arg_str(args, "text").ok_or("缺少 TEXT")?;
            let base = match arg_str(args, "path") {
                Some(p) => resolve_path(&p),
                None => workspace_path(),
            };
            let base = ensure_in_workspace(&base)?;
            if !base.is_dir() {
                return Err(format!("不是文件夹：{}", workspace_relative(&base)));
            }
            let mut lines_out: Vec<String> = Vec::new();
            let needle_lower = needle.to_lowercase();
            walk_files(&base, 0, &mut |path| {
                if lines_out.len() >= 100 {
                    return false;
                }
                let Ok(meta) = std::fs::metadata(path) else {
                    return true;
                };
                if meta.len() > 1_000_000 {
                    return true;
                }
                let Ok(content) = std::fs::read_to_string(path) else {
                    return true;
                };
                let rel = path
                    .strip_prefix(&base)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                for (i, line) in content.lines().enumerate() {
                    if line.to_lowercase().contains(&needle_lower) {
                        lines_out.push(format!("{rel}:{}: {}", i + 1, line.trim_end()));
                        if lines_out.len() >= 100 {
                            break;
                        }
                    }
                }
                true
            });
            let mut out = format!("搜索 \"{needle}\"：{} 处匹配\n", lines_out.len());
            for l in &lines_out {
                out.push_str(l);
                out.push('\n');
            }
            if lines_out.len() >= 100 {
                out.push_str("…（达到 100 条上限）\n");
            }
            Ok(truncate_output(&out))
        }
        "run" => {
            let command = arg_str(args, "cmd").ok_or("缺少 CMD")?;
            validate_command(&command)?;

            let cwd = match arg_str(args, "path") {
                Some(p) => resolve_path(&p),
                None => workspace_path(),
            };
            let cwd = ensure_in_workspace(&cwd)?;
            if !cwd.is_dir() {
                return Err(format!("不是文件夹：{}", cwd.display()));
            }
            let cwd = cwd.display().to_string();
            let (output, code) = psh_run(&command, Some(cwd.as_str()))?;
            Ok(match code {
                Some(c) => format!("{output}\n[退出码 {c}]"),
                None => output,
            })
        }
        other => Err(format!("未知工具：{other}")),
    }
}

/// 递归遍历文件（跳过常见大目录），visitor 返回 false 时停止
fn walk_files(dir: &Path, depth: usize, visitor: &mut dyn FnMut(&Path) -> bool) {
    if depth > 8 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if matches!(
                name.as_str(),
                ".git" | "node_modules" | "target" | ".idea" | ".vscode" | "dist" | "build"
            ) {
                continue;
            }
            walk_files(&path, depth + 1, visitor);
        } else if !visitor(&path) {
            return;
        }
    }
}

""