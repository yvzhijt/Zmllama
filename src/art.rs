// ── 深海女仆工坊 · 美术资源 ─────────────────────────────────────
// 立绘/宫殿/蕾丝等素材在编译期内嵌，运行时转成 data URI 注入 CSS，
// 免去外部协议与文件分发。

use base64::Engine;

const PALACE_NIGHT: &[u8] = include_bytes!("../res/skin/palace-night.webp");
const PALACE_DAY: &[u8] = include_bytes!("../res/skin/palace-day.webp");
const MAID_LEFT: &[u8] = include_bytes!("../res/skin/maid-left.webp");
const MAID_RIGHT: &[u8] = include_bytes!("../res/skin/maid-right.webp");
const LACE: &[u8] = include_bytes!("../res/skin/lace-tile.png");
const TRIM: &[u8] = include_bytes!("../res/skin/trim-tile.webp");
const CREST: &[u8] = include_bytes!("../res/skin/crest.webp");

fn uri(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// `:root` 层的美术变量，由 `main.rs` 以 `<style>` 注入。
pub fn art_css() -> String {
    format!(
        ":root{{--art-palace-night:url(\"{}\");--art-palace-day:url(\"{}\");\
--art-maid-left:url(\"{}\");--art-maid-right:url(\"{}\");\
--art-lace:url(\"{}\");--art-trim:url(\"{}\");--art-crest:url(\"{}\")}}",
        uri("image/webp", PALACE_NIGHT),
        uri("image/webp", PALACE_DAY),
        uri("image/webp", MAID_LEFT),
        uri("image/webp", MAID_RIGHT),
        uri("image/png", LACE),
        uri("image/webp", TRIM),
        uri("image/webp", CREST),
    )
}
