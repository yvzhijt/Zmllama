mod agent;
mod llm;
mod markdown;
mod panel;
mod protocol;
pub mod shell;
mod state;
mod tools;

pub use panel::TaskContent;
pub use state::{set_workspace, workspace_path};

pub fn shell_sync_cwd(dir: String) -> Result<String, String> {
    let (out, code) = shell::psh_run(&format!("Set-Location -LiteralPath '{}'", dir.replace('\'', "''")), None)?;
    match code {
        Some(c) => Ok(format!("{out}\n[退出码 {c}]")),
        None => Ok(out),
    }
}

pub const LLM_BASE: &str = "http://localhost:8080/v1";

static CACHED_MODEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub async fn detect_model() -> Result<String, String> {
    if let Some(cached) = CACHED_MODEL.get() {
        return Ok(cached.clone());
    }
    let client = &state::HTTP_CLIENT;
    let resp = client
        .get(format!("{LLM_BASE}/models"))
        .send()
        .await
        .map_err(|e| format!("无法连接 llama-server：{e}"))?;

    if !resp.status().is_success() {
        return Err(format!("获取模型列表失败：{}", resp.status()));
    }

    let models: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析模型列表失败：{e}"))?;
    let model = models["data"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|m| m["id"].as_str())
        .map(String::from)
        .ok_or_else(|| "服务器未返回任何模型".to_string())?;
    let _ = CACHED_MODEL.set(model.clone());
    Ok(model)
}

/// 阻塞任务包装
pub(crate) async fn run_blocking<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.await {
        Ok(v) => v,
        Err(_) => panic!("blocking task dropped"),
    }
}

/// 截断输出：UI 展示用，保留较多内容
pub(crate) fn truncate_output(s: &str) -> String {
    const MAX: usize = 4000;
    let count = s.chars().count();
    if count <= MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(2500).collect();
    let tail: String = s.chars().skip(count.saturating_sub(1000)).collect();
    format!(
        "{head}\n…(省略 {omitted} 字符)…\n{tail}",
        omitted = count - 3500
    )
}

/// 截断输出：回灌给模型的上下文用，比 UI 更紧
/// （1500 字符约等于 4B 模型能稳定读完的量，再长会丢掉开头）
pub(crate) fn truncate_for_context(s: &str) -> String {
    const MAX: usize = 1500;
    let count = s.chars().count();
    if count <= MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(1000).collect();
    let tail: String = s.chars().skip(count.saturating_sub(400)).collect();
    format!(
        "{head}\n…(省略 {omitted} 字符)…\n{tail}",
        omitted = count - 1400
    )
}