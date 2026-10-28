use dioxus::prelude::*;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq)]
struct DirEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    children: Vec<DirEntry>,
}

fn read_dir_sorted(path: &PathBuf) -> Vec<DirEntry> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return vec![];
    };
    let mut dirs = vec![];
    let mut files = vec![];
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let entry = DirEntry {
            name,
            path: e.path(),
            is_dir,
            children: vec![],
        };
        if is_dir {
            dirs.push(entry);
        } else {
            files.push(entry);
        }
    }
    dirs.sort_by_key(|a| a.name.to_lowercase());
    files.sort_by_key(|a| a.name.to_lowercase());
    dirs.extend(files);
    dirs
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

#[component]
pub fn FileTree(workspace: Signal<String>) -> Element {
    let mut open = use_signal(|| false);
    let mut refresh_key = use_signal(|| 0u32);

    let root = use_memo(move || {
        let _ = refresh_key();
        let p = PathBuf::from(workspace());
        if !p.exists() {
            return vec![];
        }
        read_dir_sorted(&p)
    });
    let name = use_memo(move || dir_name(&PathBuf::from(workspace())));

    rsx! {
        div { class: "filetree-panel",

            // 标题栏
            div { class: "ft-header",
                button {
                    class: "ft-toggle",
                    onclick: move |_| open.toggle(),
                    if open() { "▾" } else { "▸" }
                    span { class: "ft-title", "📁 {name}" }
                }
                button {
                    class: "ft-refresh",
                    onclick: move |_| refresh_key.set(refresh_key() + 1),
                    "↻"
                }
            }

            if open() {
                div { class: "ft-body",
                    for entry in root() {
                        FileNode { entry, depth: 1 }
                    }
                }
            }
        }
    }
}

#[component]
fn FileNode(entry: DirEntry, depth: usize) -> Element {
    let mut expanded = use_signal(|| false);

    if entry.is_dir {
        let has_children = use_memo(move || {
            if !expanded() {
                return vec![];
            }
            read_dir_sorted(&entry.path)
        });

        rsx! {
            div { class: "ft-item ft-dir",
                style: "padding-left: {12 + (depth - 1) * 14}px",
                onclick: move |_| expanded.toggle(),
                span { class: "ft-arrow", if expanded() { "▾" } else { "▸" } }
                span { class: "ft-name", "📁 {entry.name}" }
            }
            if expanded() {
                for child in has_children() {
                    FileNode { entry: child, depth: depth + 1 }
                }
            }
        }
    } else {
        rsx! {
            div { class: "ft-item ft-file",
                style: "padding-left: {12 + (depth - 1) * 14 + 14}px",
                title: "{entry.path.display()}",
                span { class: "ft-name", "📄 {entry.name}" }
            }
        }
    }
}