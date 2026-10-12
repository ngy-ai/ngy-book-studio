use std::{collections::HashMap, io::Cursor, ops::Range, sync::Arc};

use anyhow::{Context as _, Result, bail, ensure};
use percent_encoding::percent_decode_str;
use rbook::{Epub, epub::rewrite::EpubRewriteOptions};

use crate::{
    document::{AssetRole, BookDocument},
    media::{MediaBackend, MediaMetadata, MediaResponse, MediaService},
    theme::{Appearance, Palette},
};

/// Reading text size in pixels: what a chapter renders at before the reader
/// changes it, the range Ctrl + wheel covers, and one wheel notch's step. The
/// chapter runtime clamps the same range in `READER_INITIALIZATION_SCRIPT`, and
/// `the_chapter_runtime_shares_the_host_text_size_range` fails if they drift.
pub const READER_FONT_SIZE_DEFAULT: u8 = 18;
pub const READER_FONT_SIZE_MIN: u8 = 14;
pub const READER_FONT_SIZE_MAX: u8 = 30;
pub const READER_FONT_SIZE_STEP: u8 = 2;

/// 章节的基线样式表。这里的自定义属性是**兜底**，取值与 `theme::Palette::LIGHT`
/// 一致：章节由 WebView 直接渲染，宿主只能靠 `reader_appearance_script` 在运行时
/// 覆盖成当前皮肤。两处宁可重复一份数值，也不要让"脚本没跑到"变成一页不可读的正文。
pub(crate) const READER_CSS: &str = r#"
  /* ngy-reader-style */
  :root {
    color-scheme: light;
    --ngy-font-size: 18px;
    --ngy-paper: #f7f5f0;
    --ngy-surface: #fffefa;
    --ngy-text: #292621;
    --ngy-muted: #6f6860;
    --ngy-border: #e1ddd5;
    --ngy-accent: #b05538;
    --ngy-accent-strong: #9a4a30;
    --ngy-accent-soft: #f1ddd5;
  }
  html { background: var(--ngy-paper) !important; }
  body {
    box-sizing: border-box !important;
    max-width: 820px !important;
    min-height: 100vh !important;
    margin: 0 auto !important;
    padding: 48px 72px 96px !important;
    background: var(--ngy-paper) !important;
    color: var(--ngy-text) !important;
    /* 西文衬线在前：拉丁字符用 Georgia / Times New Roman，不被 CJK 字体的
       西文字形接管，这与书作者在书内 CSS 里的常见选择一致。中文字体按
       “真实存在且字重正规”排序：Noto Serif SC（可变字体）→ Noto Serif CJK SC
       → SimSun；Source Han Serif SC 必须排在 SimSun 之后——有些机器只装了
       它的 Heavy(900) 字重，一旦命中就是全篇超粗的“黑体加粗”。 */
    font-family: Georgia, "Times New Roman", "Noto Serif SC", "Noto Serif CJK SC", SimSun, "Source Han Serif SC", "Microsoft YaHei", serif !important;
    font-size: var(--ngy-font-size) !important;
    font-weight: normal !important;
    line-height: 1.85 !important;
    overflow-wrap: anywhere;
  }
  p { margin: 0 0 1.15em !important; }
  h1, h2, h3, h4, h5, h6 {
    color: var(--ngy-text) !important;
    line-height: 1.35 !important;
    margin-top: 1.7em !important;
  }
  h1:first-child, h2:first-child, h3:first-child { margin-top: 0 !important; }
  img, svg, video { max-width: 100% !important; height: auto !important; }
  table { max-width: 100% !important; border-collapse: collapse; }
  a { color: var(--ngy-accent) !important; text-decoration-thickness: 1px; }
  blockquote {
    margin-left: 0 !important;
    padding-left: 1.2em !important;
    border-left: 3px solid var(--ngy-border) !important;
    color: var(--ngy-muted) !important;
  }
  @media (max-width: 720px) {
    body { padding: 32px 34px 72px !important; }
  }
"#;

mod auth;

mod book;

mod resource;

mod util;

pub use auth::*;

pub use book::*;

pub use resource::*;

pub use util::*;

#[cfg(test)]
mod tests;
