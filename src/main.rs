#![windows_subsystem = "windows"]

use dioxus::desktop::muda::{
    accelerator::{Accelerator, Code, Modifiers},
    CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu,
};
use dioxus::desktop::use_muda_event_handler;
use dioxus::desktop::{LogicalPosition, LogicalSize};
use dioxus::prelude::*;
use futures_channel::oneshot;
use std::cell::RefCell;

mod task;
mod terminal;

use task::TaskContent;
use terminal::TerminalPanel;

// 主题菜单项句柄（CheckMenuItem 是 Rc 类型，仅主线程使用）
thread_local! {
    static THEME_ITEMS: RefCell<(Option<CheckMenuItem>, Option<CheckMenuItem>)> =
        const { RefCell::new((None, None)) };
}

fn build_menu() -> Menu {
    let menu = Menu::new();

    let file = Submenu::new("文件", true);
    let open = MenuItem::with_id(
        "open-folder",
        "打开文件夹...",
        true,
        Some(Accelerator::new(
            Some(Modifiers::CONTROL),
            Code::KeyO,
        )),
    );
    let quit = MenuItem::with_id(
        "quit-app",
        "退出",
        true,
        Some(Accelerator::new(
            Some(Modifiers::CONTROL),
            Code::KeyQ,
        )),
    );
    file.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])
        .unwrap();

    let edit = Submenu::new("编辑", true);
    let font_inc = MenuItem::with_id(
        "font-inc",
        "字号 +",
        true,
        Some(Accelerator::new(
            Some(Modifiers::CONTROL),
            Code::Equal,
        )),
    );
    let font_dec = MenuItem::with_id(
        "font-dec",
        "字号 -",
        true,
        Some(Accelerator::new(
            Some(Modifiers::CONTROL),
            Code::Minus,
        )),
    );
    edit.append_items(&[&font_inc, &font_dec])
        .unwrap();

    let win = Submenu::new("窗口", true);
    let theme_dark = CheckMenuItem::with_id(
        "theme-dark",
        "暗色主题",
        true,
        true,
        None::<Accelerator>,
    );
    let theme_light = CheckMenuItem::with_id(
        "theme-light",
        "亮色主题",
        true,
        false,
        None::<Accelerator>,
    );
    win.append_items(&[&theme_dark, &theme_light])
        .unwrap();
    THEME_ITEMS.with(|c| {
        *c.borrow_mut() = (Some(theme_dark), Some(theme_light));
    });

    menu.append_items(&[&file, &edit, &win]).unwrap();
    menu
}

fn main() {
    let icon_data = include_bytes!("../1.ico");
    let icon = image::load_from_memory(icon_data)
        .ok()
        .and_then(|img| {
            let img = img.into_rgba8();
            let (w, h) = img.dimensions();
            dioxus::desktop::tao::window::Icon::from_rgba(img.into_raw(), w, h).ok()
        });

    let window = dioxus::desktop::WindowBuilder::new()
        .with_title("Zmllama")
        // 显式位置/尺寸：dioxus debug 会从 %TEMP%\window-state.json 恢复窗口状态，
        // 若上次是最小化退出会还原到 (-32000,-32000) 屏外不可见；显式设置可绕过恢复
        .with_inner_size(LogicalSize::new(1400.0, 900.0))
        .with_min_inner_size(LogicalSize::new(880.0, 540.0))
        .with_position(LogicalPosition::new(80.0, 60.0))
        .with_window_icon(icon);

    let menu = build_menu();

    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            dioxus::desktop::Config::new()
                .with_window(window)
                .with_menu(menu)
                .with_custom_head(
                    "<script>document.addEventListener('contextmenu',e=>e.preventDefault())</script>"
                        .to_string(),
                ),
        )
        .launch(app);
}

fn app() -> Element {
    let mut left_pct = use_signal(|| 50.0_f64);
    let mut dragging = use_signal(|| false);
    let mut view_w = use_signal(|| 1200.0_f64);
    let mut dark = use_signal(|| true);
    let mut font_size = use_signal(|| 14u32);
    let mut workspace = use_signal(|| task::workspace_path().display().to_string());

    use_muda_event_handler(move |event| {
        if event.id() == "open-folder" {
            let (tx, rx) = oneshot::channel();
            std::thread::spawn(move || {
                let path = rfd::FileDialog::new()
                    .set_title("选择文件夹")
                    .pick_folder();
                let _ = tx.send(path);
            });
            spawn(async move {
                if let Ok(Some(path)) = rx.await {
                    // 同步任务模式工作区
                    task::set_workspace(path.clone());
                    workspace.set(path.display().to_string());
                    // 同时切换任务持久 shell 和终端工作目录
                    let escaped = path.to_string_lossy().replace('\'', "''");
                    let _ = terminal::inject_shell_command(format!(
                        "\r\nSet-Location -LiteralPath '{}'\r\n",
                        escaped
                    ));
                    let _ = task::shell_sync_cwd(path.display().to_string());
                }
            });
        } else if event.id() == "quit-app" {
            std::process::exit(0);
        } else if event.id() == "theme-dark" {
            dark.set(true);
            terminal::set_theme(true);
            THEME_ITEMS.with(|c| {
                let (d, l) = &*c.borrow();
                if let Some(i) = d {
                    i.set_checked(true);
                }
                if let Some(i) = l {
                    i.set_checked(false);
                }
            });
        } else if event.id() == "theme-light" {
            dark.set(false);
            terminal::set_theme(false);
            THEME_ITEMS.with(|c| {
                let (d, l) = &*c.borrow();
                if let Some(i) = d {
                    i.set_checked(false);
                }
                if let Some(i) = l {
                    i.set_checked(true);
                }
            });
        } else if event.id() == "font-inc" {
            let new_size = (font_size() + 1).min(32);
            font_size.set(new_size);
            terminal::set_font_size(new_size);
        } else if event.id() == "font-dec" {
            let new_size = font_size().saturating_sub(1).max(8);
            font_size.set(new_size);
            terminal::set_font_size(new_size);
        }
    });

    rsx! {
        style { {CSS} }
        div {
            class: if dark() { "app-root theme-dark" } else { "app-root theme-light" },

            main {
                class: format!("main-container{}", if dragging() { " dragging" } else { "" }),
                onmousemove: move |evt| {
                    if dragging() {
                        let x = evt.data.client_coordinates().x;
                        let pct = (x / view_w()) * 100.0;
                        left_pct.set(pct.clamp(15.0, 85.0));
                    }
                },
                onmouseup: move |_| dragging.set(false),
                onmouseleave: move |_| dragging.set(false),

                TerminalPanel { width_pct: left_pct(), dark_theme: dark() }

                div {
                    class: "divider",
                    onmousedown: move |_| {
                        dragging.set(true);
                        spawn(async move {
                            if let Ok(v) = document::eval("return window.innerWidth;").await {
                                if let Some(w) = v.as_f64() {
                                    view_w.set(w);
                                }
                            }
                        });
                    },
                    div { class: "divider-line" }
                }

                RightPanel { workspace }
            }
        }
    }
}

#[component]
fn RightPanel(workspace: Signal<String>) -> Element {
    rsx! {
        div { class: "chat-panel",
            TaskContent { workspace }
        }
    }
}

const CSS: &str = r#"
* {
    margin: 0;
    padding: 0;
    box-sizing: border-box;
}

html, body {
    width: 100%;
    height: 100%;
    overflow: hidden;
    font-family: "Segoe UI", "Microsoft YaHei", "PingFang SC", "Noto Sans SC", sans-serif;
    background: #11111b;
}

/* ── 主题变量 ───────────────────────────────────────────── */
.app-root {
    position: fixed;
    inset: 0;
    display: flex;
    flex-direction: column;
    color: var(--text);
    background: var(--bg);
}

.app-root.theme-dark {
    --bg: #11111b;
    --panel: #1e1e2e;
    --panel-2: #181825;
    --border: #313244;
    --border-2: #45475a;
    --text: #cdd6f4;
    --text-dim: #a6adc8;
    --text-muted: #6c7086;
    --accent: #89b4fa;
    --term-bg: #11111b;
    --input-bg: #313244;
    --thumb: #45475a;
    --shadow: rgba(0, 0, 0, 0.35);
}

.app-root.theme-light {
    --bg: #e6e9ef;
    --panel: #eff1f5;
    --panel-2: #dce0e8;
    --border: #ccd0da;
    --border-2: #bcc0cc;
    --text: #4c4f69;
    --text-dim: #5c5f77;
    --text-muted: #8c8fa1;
    --accent: #1e66f5;
    --term-bg: #eff1f5;
    --input-bg: #e6e9ef;
    --thumb: #bcc0cc;
    --shadow: rgba(0, 0, 0, 0.12);
}

/* ── 布局 ───────────────────────────────────────────────── */
.main-container {
    display: flex;
    flex-direction: row;
    flex: 1;
    min-height: 0;
    position: relative;
    padding: 8px 10px 10px;
}

.main-container.dragging,
.main-container.dragging * {
    cursor: col-resize !important;
    user-select: none;
}

.terminal-panel,
.chat-panel {
    display: flex;
    flex-direction: column;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
    min-width: 0;
    min-height: 0;
    box-shadow: 0 4px 20px var(--shadow);
}

.chat-panel {
    flex: 1 1 0;
    min-height: 0;
}

.divider {
    flex: 0 0 14px;
    display: flex;
    align-items: stretch;
    justify-content: center;
    cursor: col-resize;
}

.divider-line {
    width: 2px;
    margin: 14px 0;
    border-radius: 2px;
    background: var(--border);
    transition: background 0.15s, width 0.15s;
}

.divider:hover .divider-line {
    width: 3px;
    background: var(--border-2);
}

.main-container.dragging .divider-line {
    width: 3px;
    background: var(--accent);
}

/* ── 终端 ───────────────────────────────────────────────── */
.terminal-body {
    flex: 1;
    min-height: 0;
    overflow: hidden;
    background: var(--term-bg);
    /* 必须 relative，否则 #terminals/.term-instance 的 absolute 会跑出面板 */
    position: relative;
}

/* ── 标签栏 ─────────────────────────────────────────────── */
.tab-bar {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 4px 8px 0;
    background: var(--panel-2);
    border-bottom: 1px solid var(--border);
    overflow-x: auto;
    flex-shrink: 0;
    min-height: 32px;
}

.tab-item {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 12px 5px 14px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-bottom: none;
    border-radius: 8px 8px 0 0;
    font-size: 12px;
    color: var(--text-dim);
    cursor: pointer;
    user-select: none;
    white-space: nowrap;
    transition: background 0.12s, color 0.12s, border-color 0.12s;
}

.tab-item:hover {
    background: var(--border);
    color: var(--text);
}

.tab-item.active {
    background: var(--term-bg);
    color: var(--accent);
    border-color: var(--border-2);
}

.tab-label {
    pointer-events: none;
}

.tab-close {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    padding: 0 2px;
    border-radius: 3px;
}

.tab-close:hover {
    background: #f38ba8;
    color: #11111b;
}

.tab-add {
    background: none;
    border: 1px dashed var(--border-2);
    color: var(--text-dim);
    font-size: 15px;
    line-height: 1;
    cursor: pointer;
    padding: 3px 10px;
    border-radius: 6px;
    margin-left: 4px;
    flex-shrink: 0;
    transition: border-color 0.12s, color 0.12s;
}

.tab-add:hover {
    border-color: var(--accent);
    color: var(--accent);
}

.terminal-body .xterm {
    padding: 4px 2px;
}

#terminals {
    position: absolute;
    inset: 0;
}

.term-instance {
    position: absolute;
    inset: 0;
}

.term-instance .xterm {
    /* xterm 根据 rows*cellHeight 自己算高度，不要强制 100% */
    height: auto !important;
    padding: 4px 2px;
}

.terminal-body .xterm-viewport {
    scrollbar-width: thin;
    scrollbar-color: var(--thumb) var(--term-bg);
}

.terminal-body .xterm-viewport::-webkit-scrollbar {
    width: 6px;
}

.terminal-body .xterm-viewport::-webkit-scrollbar-track {
    background: var(--term-bg);
}

.terminal-body .xterm-viewport::-webkit-scrollbar-thumb {
    background: var(--thumb);
    border-radius: 3px;
}

/* ── 对话 ───────────────────────────────────────────────── */
.chat-messages {
    flex: 1;
    padding: 16px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
    scrollbar-width: thin;
    scrollbar-color: var(--thumb) var(--panel);
}

.chat-messages::-webkit-scrollbar {
    width: 6px;
}

.chat-messages::-webkit-scrollbar-track {
    background: transparent;
}

.chat-messages::-webkit-scrollbar-thumb {
    background: var(--thumb);
    border-radius: 3px;
}

.chat-empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    color: var(--text-muted);
    user-select: none;
}

.chat-empty-title {
    font-size: 15px;
    font-weight: 600;
    color: var(--text-dim);
}

.chat-empty-hint {
    font-size: 12.5px;
}

.msg {
    max-width: 82%;
    padding: 9px 13px;
    font-size: 13.5px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-word;
}

.msg-role {
    display: block;
    font-size: 11px;
    font-weight: 600;
    margin-bottom: 3px;
    opacity: 0.7;
    letter-spacing: 0.5px;
}

.msg.user {
    align-self: flex-end;
    background: linear-gradient(135deg, #89b4fa, #74c7ec);
    color: #1e1e2e;
    border-radius: 14px 14px 4px 14px;
    box-shadow: 0 2px 8px rgba(137, 180, 250, 0.25);
}

.msg.assistant {
    align-self: flex-start;
    background: var(--input-bg);
    color: var(--text);
    border-radius: 14px 14px 14px 4px;
    border: 1px solid var(--border-2);
}

.chat-input-area {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 12px 14px 14px;
    border-top: 1px solid var(--border);
    background: var(--panel-2);
}

.chat-input {
    width: 100%;
    min-height: 64px;
    max-height: 160px;
    padding: 10px 12px;
    font-size: 13.5px;
    font-family: inherit;
    line-height: 1.5;
    background: var(--input-bg);
    color: var(--text);
    border: 1px solid var(--border-2);
    border-radius: 10px;
    resize: vertical;
    outline: none;
    transition: border-color 0.15s, box-shadow 0.15s;
}

.chat-input::placeholder {
    color: var(--text-muted);
}

.chat-input:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px rgba(137, 180, 250, 0.15);
}

.chat-send {
    width: 42px;
    height: 42px;
    padding: 0;
    font-size: 18px;
    font-family: inherit;
    font-weight: bold;
    background: var(--accent);
    color: #1e1e2e;
    border: none;
    border-radius: 10px;
    cursor: pointer;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: filter 0.15s, transform 0.1s;
}

.chat-send:hover:not(:disabled) {
    filter: brightness(1.15);
}

.chat-send:active:not(:disabled) {
    transform: scale(0.93);
}

.chat-send:disabled {
    opacity: 0.4;
    cursor: not-allowed;
}

.chat-stop {
    width: 42px;
    height: 42px;
    padding: 0;
    font-size: 16px;
    font-family: inherit;
    font-weight: bold;
    background: #f38ba8;
    color: #1e1e2e;
    border: none;
    border-radius: 10px;
    cursor: pointer;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: filter 0.15s, transform 0.1s;
}

.chat-stop:hover:not(:disabled) {
    filter: brightness(1.15);
}

.chat-stop:active:not(:disabled) {
    transform: scale(0.93);
}

.chat-stop:disabled {
    opacity: 0.4;
    cursor: not-allowed;
}

/* ── 右侧面板 ───────────────────────────────────────────── */
.task-info {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 14px;
    font-size: 11.5px;
    color: var(--text-muted);
    background: var(--panel-2);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
}

.task-workspace {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.task-status {
    flex-shrink: 0;
}

.mini-btn {
    flex-shrink: 0;
    padding: 2px 9px;
    font: inherit;
    font-size: 11px;
    background: transparent;
    border: 1px solid var(--border-2);
    color: var(--text-dim);
    border-radius: 6px;
    cursor: pointer;
    transition: border-color 0.12s, color 0.12s;
}

.mini-btn:hover {
    border-color: var(--accent);
    color: var(--accent);
}

.tool-card {
    align-self: stretch;
    max-width: 100%;
    padding: 8px 10px;
    font-size: 12px;
    background: var(--panel-2);
    border: 1px solid var(--border-2);
    border-radius: 10px;
}

.tool-card-head {
    display: flex;
    align-items: center;
    gap: 8px;
}

.tool-name {
    font-weight: 600;
    color: var(--accent);
    font-family: Consolas, "Cascadia Mono", monospace;
}

.tool-state {
    font-size: 11px;
    padding: 1px 7px;
    border-radius: 999px;
    border: 1px solid currentColor;
}

.tool-state.running {
    color: #f9e2af;
}

.tool-state.ok {
    color: #a6e3a1;
}

.tool-state.err {
    color: #f38ba8;
}

.tool-state.denied {
    color: var(--text-muted);
}

.tool-args {
    margin-top: 6px;
    font-family: Consolas, "Cascadia Mono", monospace;
    font-size: 11px;
    color: var(--text-muted);
    white-space: pre-wrap;
    word-break: break-all;
}

.tool-output {
    margin-top: 6px;
    max-height: 220px;
    overflow: auto;
    padding: 8px;
    font-family: Consolas, "Cascadia Mono", monospace;
    font-size: 11.5px;
    line-height: 1.45;
    color: var(--text-dim);
    background: var(--term-bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    white-space: pre-wrap;
    word-break: break-word;
    scrollbar-width: thin;
    scrollbar-color: var(--thumb) var(--term-bg);
}

.approval-card {
    align-self: stretch;
    padding: 10px 12px;
    background: rgba(249, 226, 175, 0.08);
    border: 1px solid #f9e2af;
    border-radius: 10px;
}

.approval-title {
    margin-bottom: 6px;
    font-size: 12.5px;
    font-weight: 600;
    color: #f9e2af;
}

.approval-detail {
    max-height: 200px;
    overflow: auto;
    padding: 8px;
    font-family: Consolas, "Cascadia Mono", monospace;
    font-size: 11.5px;
    color: var(--text-dim);
    background: var(--term-bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    white-space: pre-wrap;
    word-break: break-word;
}

.approval-btns {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 8px;
}

.btn-allow,
.btn-deny {
    padding: 5px 16px;
    font: inherit;
    font-size: 12.5px;
    font-weight: 600;
    border-radius: 7px;
    cursor: pointer;
}

.btn-allow {
    background: #a6e3a1;
    color: #1e1e2e;
    border: none;
}

.btn-deny {
    background: transparent;
    border: 1px solid var(--border-2);
    color: var(--text-dim);
}

.btn-allow:hover {
    filter: brightness(1.08);
}

.btn-deny:hover {
    border-color: #f38ba8;
    color: #f38ba8;
}
"#;