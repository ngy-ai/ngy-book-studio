# 代码地图：派生页面渲染与 Office

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 模块

- `src/preview/`、`src/windows_pdf_renderer.rs`、`src/djvu_renderer.rs`：`VisualRenderer`、
  结构化页面 PNG 光栅化、Windows PDF 原页光栅化与纯 Rust DjVu 逐页光栅化、
  可暂停/恢复/重试/取消的持久任务和本地 PDF.js 资产路由。DjVu 渲染器（`ngy-djvu-png`）
  不加 `cfg(windows)`，页与内容单元严格 1:1（`content_unit_id` + `SourceLocator::DjvuPage`），
  必须在 `db::transactions::{renderer_for_source, visual_job_spec}` 里按 `format == "djvu"`
  选中；新增 renderer 时这三处（注册、恢复选择、任务构造）必须同时更新，否则导入时
  创建的 `visual-render:{source_id}` 任务会退回结构化 SVG 渲染。
  暂停的结清与索引任务同一套规则（2026-10-17，见 `code-map-ai.md`）：
  `VisualJobStore::request_pause` 对排队中的记录当场写成 `Paused`（SQLite 与内存两个存储
  都必须如此），`recover_interrupted` 把没有执行者能观察的 `pause_requested` 一并结清；
  只留标志会让这行永远「正在暂停」且界面不给任何操作。
  渲染器选择失败/未注册会在恢复期报「找不到当前来源所需的 renderer」，不要用
  `ngy-structural-png` 兜底掩盖 DjVu 页面缺失。
- `src/office_com.rs`、`src/office_preview/`、`src/office_visual.rs`：可选 Office STA
  工作者、只读临时导出和持久视觉渲染器；原件禁用宏和外链更新，派生页按当前
  renderer/revision/profile 发布，失败时回退结构化预览。
- `src/reader.rs`、`src/epub_limits.rs`：EPUB 投影、资源授权、导航 URL 和归档安全上限。
- `src/ui/page_image/`：页面图片窗口（DjVu 与 Office 逐页预览共用）。`text.rs` 的
  `page_text_paragraphs` 是显示与朗读共用的分段：面板 HTML 与朗读块按同一个段落索引
  对齐，`page_text_html` 给当前朗读段包 `<mark>`，两处切分不得分叉。`speech.rs` 接
  `src/ui/read_aloud.rs` 的共享朗读层；翻页与关闭窗口停止朗读。Office 逐页预览是图片页、
  没有文字层，不提供朗读按钮（结构化阅读在阅读窗口，那里已支持朗读）。