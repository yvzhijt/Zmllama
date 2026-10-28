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

mod art;
mod filetree;
mod task;
mod terminal;

use filetree::FileTree;
use task::TaskContent;
use terminal::TerminalPanel;

// ── 配置持久化 ─────────────────────────────────────────────────

/// 应用配置（主题、字号、工作区）
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct AppConfig {
    dark: bool,
    workspace: Option<String>,
}

fn config_path() -> std::path::PathBuf {
    let dir = std::env::var("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default())
        .join("Zmllama");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("config.json")
}

fn load_config() -> AppConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(cfg: &AppConfig) {
    if let Ok(s) = serde_json::to_string(cfg) {
        let _ = std::fs::write(config_path(), s);
    }
}

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

    menu.append_items(&[&file, &win]).unwrap();
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
    let cfg = load_config();
    let mut left_pct = use_signal(|| 40.0_f64);
    let mut dragging = use_signal(|| false);
    let mut view_w = use_signal(|| 1200.0_f64);
    let mut dark = use_signal(|| cfg.dark);
    let mut workspace = use_signal(|| {
        let p = cfg
            .workspace
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(task::workspace_path);
        let p = if p.exists() { p } else { task::workspace_path() };
        task::set_workspace(p.clone());
        p.display().to_string()
    });

    use_effect(move || {
        // 启动时把菜单勾选同步到已保存的主题
        let is_dark = dark();
        THEME_ITEMS.with(|c| {
            let (d, l) = &*c.borrow();
            if let Some(i) = d {
                i.set_checked(is_dark);
            }
            if let Some(i) = l {
                i.set_checked(!is_dark);
            }
        });
    });

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
                        "Set-Location -LiteralPath '{}'\r",
                        escaped
                    ));
                    let _ = task::shell_sync_cwd(path.display().to_string());
                    save_config(&AppConfig {
                        dark: dark(),
                        workspace: Some(path.display().to_string()),
                    });
                }
            });
        } else if event.id() == "quit-app" {
            std::process::exit(0);
        } else if event.id() == "theme-dark" {
            dark.set(true);
            terminal::set_theme(true);
            save_config(&AppConfig {
                dark: true,
                workspace: workspace().into(),
            });
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
            save_config(&AppConfig {
                dark: false,
                workspace: workspace().into(),
            });
            THEME_ITEMS.with(|c| {
                let (d, l) = &*c.borrow();
                if let Some(i) = d {
                    i.set_checked(false);
                }
                if let Some(i) = l {
                    i.set_checked(true);
                }
            });
        }
    });

    let art_css = art::art_css();

    rsx! {
        // 美术资源（立绘/宫殿/蕾丝）以 data URI 注入
        style { dangerous_inner_html: art_css }
        style { dangerous_inner_html: CSS }
        div {
            class: if dark() { "app-root theme-dark" } else { "app-root theme-light" },

            div { class: "trim-top" }

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

                // 左侧：文件树 + 终端（上下分割）
                div {
                    class: "left-panel",
                    style: "flex: 0 0 {left_pct()}%",

                    FileTree { workspace }

                    TerminalPanel { dark_theme: dark() }
                }

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
                    div { class: "divider-grip" }
                }

                RightPanel { workspace }
            }

            div { class: "trim-bottom" }
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

const CSS: &str = include_str!("skin.css");
