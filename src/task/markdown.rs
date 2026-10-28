use dioxus::prelude::*;

// ── 轻量 Markdown 渲染器 ──────────────────────────────────────
// 覆盖常见结构：围栏代码块、标题、无序/有序列表、引用、行内
// 代码/粗体/斜体/链接。足够应付智能体的文字输出，且风格可控。

#[derive(Debug, Clone)]
enum Inline {
    Text(String),
    Code(String),
    Strong(Vec<Inline>),
    Em(Vec<Inline>),
    Link { text: Vec<Inline>, href: String },
}

#[derive(Debug, Clone)]
enum Block {
    Paragraph(Vec<Inline>),
    Heading { level: u8, content: Vec<Inline> },
    Code { lang: String, content: String },
    List { ordered: bool, items: Vec<Vec<Inline>> },
    Quote(Vec<Block>),
    Hr,
}

fn flush_text(buf: &mut String, out: &mut Vec<Inline>) {
    if !buf.is_empty() {
        out.push(Inline::Text(std::mem::take(buf)));
    }
}

fn parse_inline(s: &str) -> Vec<Inline> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        // `行内代码`
        if c == '`' {
            let mut j = i + 1;
            let start = j;
            while j < n && chars[j] != '`' {
                j += 1;
            }
            if j < n {
                flush_text(&mut text, &mut out);
                out.push(Inline::Code(chars[start..j].iter().collect()));
                i = j + 1;
                continue;
            }
            text.push(c);
            i += 1;
            continue;
        }
        // **粗体**
        if c == '*' && i + 1 < n && chars[i + 1] == '*' {
            let mut j = i + 2;
            let start = j;
            while j + 1 < n && !(chars[j] == '*' && chars[j + 1] == '*') {
                j += 1;
            }
            if j + 1 < n {
                flush_text(&mut text, &mut out);
                let inner: String = chars[start..j].iter().collect();
                out.push(Inline::Strong(parse_inline(&inner)));
                i = j + 2;
                continue;
            }
        }
        // *斜体*
        if c == '*' {
            let mut j = i + 1;
            let start = j;
            while j < n && chars[j] != '*' {
                j += 1;
            }
            if j > start && j < n {
                flush_text(&mut text, &mut out);
                let inner: String = chars[start..j].iter().collect();
                out.push(Inline::Em(parse_inline(&inner)));
                i = j + 1;
                continue;
            }
        }
        // [文本](链接)
        if c == '[' {
            let mut j = i + 1;
            let start = j;
            while j < n && chars[j] != ']' {
                j += 1;
            }
            if j < n && j + 1 < n && chars[j + 1] == '(' {
                let mut k = j + 2;
                let url_start = k;
                while k < n && chars[k] != ')' {
                    k += 1;
                }
                if k < n {
                    flush_text(&mut text, &mut out);
                    let label: String = chars[start..j].iter().collect();
                    let href: String = chars[url_start..k].iter().collect();
                    out.push(Inline::Link {
                        text: parse_inline(&label),
                        href,
                    });
                    i = k + 1;
                    continue;
                }
            }
            text.push(c);
            i += 1;
            continue;
        }
        text.push(c);
        i += 1;
    }
    flush_text(&mut text, &mut out);
    out
}

fn is_bullet(t: &str) -> bool {
    t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ")
}

fn strip_bullet(t: &str) -> &str {
    t.trim_start_matches(['-', '*', '+']).trim()
}

fn is_ordered(t: &str) -> bool {
    let mut it = t.chars();
    let Some(first) = it.next() else {
        return false;
    };
    first.is_ascii_digit() && {
        let mut c = it.next();
        while let Some(ch) = c {
            if ch == '.' {
                return rest_non_empty(t);
            }
            if !ch.is_ascii_digit() {
                return false;
            }
            c = it.next();
        }
        false
    }
}

fn rest_non_empty(t: &str) -> bool {
    match t.find('.') {
        Some(p) => !t[p + 1..].trim_start().is_empty(),
        None => false,
    }
}

fn strip_ordered(t: &str) -> &str {
    match t.find('.') {
        Some(p) => t[p + 1..].trim(),
        None => t.trim(),
    }
}

fn is_hr(t: &str) -> bool {
    t.starts_with("---") || t.starts_with("***") || t.starts_with("___")
}

fn parse_heading(t: &str) -> Option<(u8, &str)> {
    let mut level = 0u8;
    for ch in t.chars() {
        if ch == '#' {
            level += 1;
            if level > 4 {
                return None;
            }
        } else {
            break;
        }
    }
    if level == 0 {
        return None;
    }
    let rest = t[level as usize..].trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((level, rest))
}

fn parse_blocks(md: &str) -> Vec<Block> {
    let lines: Vec<&str> = md.split('\n').collect();
    let n = lines.len();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < n {
        let line = lines[i];
        let trimmed = line.trim_start();

        // 围栏代码块
        if trimmed.starts_with("```") {
            let lang = trimmed.trim_start_matches('`').trim().to_string();
            let mut content = String::new();
            i += 1;
            while i < n && !lines[i].trim_start().starts_with("```") {
                content.push_str(lines[i]);
                content.push('\n');
                i += 1;
            }
            i += 1; // 跳过结束围栏
            blocks.push(Block::Code {
                lang,
                content: content.trim_end().to_string(),
            });
            continue;
        }
        // 标题
        if let Some((level, rest)) = parse_heading(trimmed) {
            blocks.push(Block::Heading {
                level,
                content: parse_inline(rest),
            });
            i += 1;
            continue;
        }
        // 列表（连续行合并为一个列表块）
        if is_bullet(trimmed) || is_ordered(trimmed) {
            let ordered = is_ordered(trimmed);
            let mut items = Vec::new();
            while i < n {
                let t = lines[i].trim_start();
                let content = if ordered {
                    is_ordered(t).then(|| strip_ordered(t))
                } else {
                    is_bullet(t).then(|| strip_bullet(t))
                };
                match content {
                    Some(c) => {
                        items.push(parse_inline(c));
                        i += 1;
                    }
                    None => {
                        if t.is_empty() {
                            break;
                        }
                        break;
                    }
                }
            }
            blocks.push(Block::List { ordered, items });
            continue;
        }
        // 引用
        if trimmed.starts_with('>') {
            let mut buf = String::new();
            while i < n {
                let t = lines[i].trim_start();
                if t.starts_with('>') {
                    let rest = t.trim_start_matches('>').trim_start().to_string();
                    buf.push_str(&rest);
                    buf.push('\n');
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(Block::Quote(parse_blocks(buf.trim_end())));
            continue;
        }
        // 分隔线
        if is_hr(trimmed) {
            blocks.push(Block::Hr);
            i += 1;
            continue;
        }
        // 空行
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        // 段落：收集到空行或下一个块开始
        let mut para = String::new();
        while i < n {
            let t = lines[i].trim();
            if t.is_empty()
                || t.starts_with("```")
                || parse_heading(t).is_some()
                || is_bullet(t)
                || is_ordered(t)
                || t.starts_with('>')
                || is_hr(t)
            {
                break;
            }
            if !para.is_empty() {
                para.push('\n');
            }
            para.push_str(t);
            i += 1;
        }
        blocks.push(Block::Paragraph(parse_inline(&para)));
    }
    blocks
}

fn render_inline(list: &[Inline]) -> Element {
    rsx! {
        for tok in list {
            match tok {
                Inline::Text(t) => rsx! { span { "{t}" } },
                Inline::Code(c) => rsx! { code { class: "md-inline-code", "{c}" } },
                Inline::Strong(inner) => rsx! { strong { {render_inline(inner)} } },
                Inline::Em(inner) => rsx! { em { {render_inline(inner)} } },
                Inline::Link { text, href } => rsx! {
                    a { class: "md-link", href: "{href}", {render_inline(text)} }
                },
            }
        }
    }
}

fn render_heading(level: u8, content: &[Inline]) -> Element {
    let body = render_inline(content);
    match level {
        1 => rsx! { h1 { class: "md-h1", {body} } },
        2 => rsx! { h2 { class: "md-h2", {body} } },
        3 => rsx! { h3 { class: "md-h3", {body} } },
        _ => rsx! { h4 { class: "md-h4", {body} } },
    }
}

fn render_list(ordered: bool, items: &[Vec<Inline>]) -> Element {
    if ordered {
        rsx! {
            ol { class: "md-list",
                for it in items {
                    li { {render_inline(it)} }
                }
            }
        }
    } else {
        rsx! {
            ul { class: "md-list",
                for it in items {
                    li { {render_inline(it)} }
                }
            }
        }
    }
}

fn render_blocks(blocks: &[Block]) -> Element {
    rsx! {
        for b in blocks {
            match b {
                Block::Paragraph(inl) => rsx! { p { class: "md-p", {render_inline(inl)} } },
                Block::Heading { level, content } => {render_heading(*level, content)}
                Block::Code { lang, content } => rsx! {
                    div { class: "md-codeblock",
                        if !lang.is_empty() {
                            div { class: "md-code-lang", "{lang}" }
                        }
                        pre { class: "md-code",
                            code { "{content}" }
                        }
                    }
                },
                Block::List { ordered, items } => {render_list(*ordered, items)}
                Block::Quote(inner) => rsx! { blockquote { class: "md-quote", {render_blocks(inner)} } },
                Block::Hr => rsx! { div { class: "md-hr" } },
            }
        }
    }
}

/// 渲染 Markdown 文本为可用的 Dioxus 元素树
#[component]
pub fn Markdown(text: String) -> Element {
    let blocks = parse_blocks(&text);
    rsx! {
        div { class: "md-body",
            {render_blocks(&blocks)}
        }
    }
}