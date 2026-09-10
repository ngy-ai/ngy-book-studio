# AGENTS.md

## 适用范围

本文件适用于仓库根目录及其所有子目录。若以后在子目录中增加更具体的
`AGENTS.md`，以距离目标文件最近的说明为准。

## 项目概览

“墨页”是一个面向 Windows 10/11 的本地图书库、阅读器与编辑器。项目使用 Rust
2024、GPUI、`gpui-component`/WebView2 和 SQLite。导入层将 EPUB、PDF、Office 与
DRM-free Kindle 文件转换为项目自有的 `BookDocument`；第三方解析器 IR 不得进入
数据库、UI 或其它公共领域模型。

当前支持导入 EPUB、PDF、DOC/DOCX、PPTX、XLSX、MOBI/AZW/AZW3，统一模型编辑，
EPUB/PDF 导出和原件导出。不要把回写原 Office/Kindle 格式、DRM、音视频转写、S3、
同步或发布打包视为已支持能力。

Windows/MSVC 是当前验收平台。依赖虽然启用了部分 Unix 图形后端，也不要据此宣称
应用已完成跨平台验证。

`ROADMAP.md` 表达实现状态和后续边界。判断当前行为时，以源码、测试、GUI 实测和
`README.md` 的一致证据为准。

## 文档职责

- `README.md` 面向使用者，只保留产品简介、用户可见功能、运行方式、本地数据行为
  和当前能力边界。
- 开发命令、测试矩阵、代码地图、实现细节和代理工作约束统一放在本文件中，不要
  再复制回 README。
- `ROADMAP.md` 记录已实现能力与明确未实现边界，不以计划勾选代替验收证据。
- 用户可见行为变化时同步更新 README 和 ROADMAP；仅实现方式或验证流程变化时更新
  本文件。

## 代码地图

- `src/main.rs`：日志、数据目录、WebView2 探测、`AppServices`/GPUI 初始化和图书库
  主窗口。
- `src/services.rs`、`src/runtime.rs`：进程级服务组合与独立 Tokio runtime；统一持有
  图书库、对象存储、格式注册表、搜索、AI、Office 和后台任务。
- `src/learning.rs`、`src/learning_records.rs`、`src/learning_catalog.rs`、
  `src/ui/learning.rs`：学习中心异步服务、逐章目录、
  独立 JSON 学习档案和原生 GPUI 训练窗口。记录不进入图书数据库；运行前保存不可变
  代码、预测与提示级别，运行报告由受信宿主生成；恢复前原样归档当前文件。
  恢复文件对话框使用后台准备的 Shell 路径：Windows 的 `\\?\` 盘符 / UNC 前缀
  只在对话框路径中转换，不改存储路径。选择器失败或取消不得锁住当前输入；实际
  保存 / 恢复失败才要求重新检测记录，已有的不确定状态不得被选择器错误清除。
  学习窗口构造编辑器前注册固定版本 `tree-sitter-python`；组件默认语言集只有 JSON，
  不能仅凭 `code_editor("python")` 或查询语言非空就判断 Python 高亮已生效。
  默认完整教程总览来自 `courses/ai-agent-tutorial/README.md`；十章正文、导读和
  课程资料属于浏览状态，不插入或重排第一章六个持久化步骤。开始 / 继续第一章
  使用保留的步骤；章节与节间导航不得丢失当前编辑，运行中不得隐藏取消控制。
  第2—10章点击“本章代码实验”后进入各章工作区；切换前异步保存旧章，保存失败保留
  输入。章节身份绑定不可变 service，十章共享各章独立的锁；档案与备份必须校验章节，
  不得将别章报告混入历史。各章存于 `learning/chapter-NN.json`，第一章原六步索引不变。
  目标章载入失败后进入其恢复状态，未加载的坏章不应阻断进入其它章。更换工作区或
  恢复备份时重建编辑器和订阅，隔离撤销历史；`InputState::set_value` 不会清除历史，
  不能让 Ctrl+Z 将上一章文字带入本章。
  可选择的只读正文使用 `scrollable_learning_text`：自然高度的 TextView 放在外层
  滚动容器内。0.5.1 的 TextView 内部虚拟列表滚动不会平移选区坐标，不得恢复
  `selectable(true).scrollable(true)` 组合。GPUI `test-support` 仅用于开发测试，
  事件与内存剪贴板回归不能替代真实 Windows 鼠标、字体和系统剪贴板验收。
- `src/document.rs`：稳定 ID 的格式无关模型，包括 `BookDocument`、`ContentUnit`、
  `BlockDocument`、`TocNode` 和 `DocumentLocator`。
- `src/formats/`：`DocumentImporter` 注册表及 EPUB、PDF、Office、Kindle 适配器；
  `office_oxide`、`ebook-rs` 等第三方类型必须在本目录内转换为统一模型。
- `src/markup.rs`、`src/editing.rs`、`src/export.rs`：HTML 解析清洗、事务式
  模型编辑，以及原件/EPUB/PDF 稳定导出。
- `src/storage.rs`、`src/media.rs`：应用自有 `BlobStore`、基于 `object_store` 的本地
  BLAKE3 内容寻址实现，以及带图书归属校验和 Range 支持的媒体响应。
- `src/library.rs`：SQLite 与对象存储之上的图书库业务编排、导入/创建/保存/删除、
  垃圾回收和兼容现有 UI 的投影。
- `src/annotations.rs`、`src/db/annotations.rs`：三种划线、人工想法与 AI 想法统一使用
  `annotations` 一张表。宿主按实际阅读章节 body 文本（排除 script/style/noscript/template，
  移除 ECMAScript 空白）的 UTF-16 起止位置校验 quote、书/单元归属及打开时的版本。
  修订或删除章节保留失效笔记，不按 quote 搜索重定位；删除图书级联清除笔记。
  同书/单元/双版本/精确起止范围的三种标记由部分唯一索引约束，改样式在事务中替换，
  不创建多条标记。删除划线只删除该范围的标记，人工与 AI 想法保持不变。
  当前开发结构版本为 13，遵循重建策略，不编写迁移。
- `src/db/`：SQLite 连接、当前结构、单表 CRUD/查询映射和跨表事务。每张表对应一个
  文件：`books.rs`、`book_sources.rs`、`content_units.rs`、`toc_entries.rs`、
  `blobs.rs`、`assets.rs`、`asset_refs.rs`、`progress.rs`、`search_chunks.rs`、
  `embeddings.rs`、`index_jobs.rs`、`visual_pages.rs`、`visual_page_staging.rs`、
  `chat_threads.rs`、`chat_messages.rs`、`chat_citations.rs`、`groups.rs`、`settings.rs`、
  `translations.rs` 和
  `office_enhancements.rs`；跨表原子操作只放 `transactions.rs`，FTS 查询放
  `book_search.rs`，建表与完整性契约放 `schema.rs`。
- `src/search.rs`、`src/indexing.rs`：作用域内 FTS5/`sqlite-vec` 精确 KNN、RRF 混合
  召回，以及可恢复的 embedding/vision 后台任务。`indexing.rs` 另实现整本图书翻译任务
  `kind="translation"`：任务标识为 `translation:<source_id>:<target_language>`，游标复用
  `next_ordinal` 作为文本块序号，逐块调用对话模型 `chat_stream` 写入 `translations` 表，
  可暂停/恢复/重试/取消；文本块按 `content_units.block_json` 的 `BlockDocument` 确定性
  展平（段落、标题、引用、列表项、表格单元格；跳过代码块和 RawHtml），以
  `(document_revision, unit_revision, target_language, 对话模型)` 判定失效并重译。目标语言
  或对话模型变化由 `AppServices::configure_translations` 经
  `transactions::reconfigure_translation_jobs` 重排；每本当前来源只保留一个目标语言任务。
  源语言（`books.language` 主语言子标签）等于目标语言时跳过；翻译任务未配置对话模型时
  失败而不猜测。模型调用不得逐 token 打日志，也不得记录正文或译文内容。
- `src/preview.rs`、`src/windows_pdf_renderer.rs`：`VisualRenderer`、结构化页面 PNG 光栅化、
  Windows PDF 原页光栅化、可暂停/恢复/重试/取消的持久任务和本地 PDF.js 资产路由。
- `src/ai.rs`、`src/credentials.rs`：OpenAI-compatible models/chat streaming/embeddings
  接口、端点策略和 Windows Credential Manager 密钥存储。
- `src/agent.rs`、`src/agent_runtime.rs`、`src/agent_chat.rs`、`src/chat.rs`：只读 Agent
  工具、SSE/tool-call 循环、窗口授权范围、会话/消息/引用持久化与对话编排。
- `src/office_com.rs`、`src/office_preview.rs`、`src/office_visual.rs`：可选 Office STA
  工作者、只读临时导出和持久视觉渲染器；原件禁用宏和外链更新，派生页按当前
  renderer/revision/profile 发布，失败时回退结构化预览。
- `src/reader.rs`、`src/epub_limits.rs`：EPUB 投影、资源授权、导航 URL 和归档安全上限。
- `src/ui/`：按窗口/职责拆分的 GPUI 界面。`library.rs`、`reader.rs`、`pdf_reader.rs`、
  `editor.rs`、`office_slides.rs` 分别管理对应窗口；`ai_sidebar.rs`、
  `ai_controller.rs`、`ai_settings.rs` 管理 AI 交互；`background_jobs.rs` 管理当前图书范围
  的派生任务；`mod.rs` 只保留跨窗口主题、窗口打开与安全关闭基础设施，其中包含按
  图书登记的窗口表：删除图书后关闭该书已打开的阅读、PDF/Office 预览和编辑窗口。
  `mod.rs` 另维护 `PdfReaderWindowRegistry`：保存“PDF 紧凑阅读”后向所有已打开的 PDF
  阅读窗口推送 `<html data-pdf-compact>`，这是唯一能枚举非单例 Office 预览窗口的登记表。
  这类关闭走各窗口的“图书已移除”路径，不写最终阅读进度、不保存草稿、不弹保存确认，
  WebView 仍在构建时等构建结束后再拆除。
  AI 回复用与 `gpui-component` 相同的固定版 `markdown` 解析器生成展示投影，按消息
  缓存；链接、图片与原始 HTML 只显示文字，不能让模型正文自动加载资源或打开 URL。
  Markdown 渲染保留自然高度和外层会话滚动，复制与持久化使用原文；来源按钮继续走
  宿主引用校验。TextView 在 flex 消息列内必须获得扣除 padding、边框和复制按钮后的
  明确像素宽度；仅用 `w_full()` 会使 Windows 上的短列表正文被裁掉，测试字体不一定
  复现。`src/ui/ai_sidebar/` 保存展示清洗和 GPUI 选择、流式更新回归。
  编辑器导航按独立 `TocNode` 树显示，节点 ID 与正文单元 ID 分开保存，不能用目录
  序号定位线性正文，也不能将同一单元的多个目录项合并。`src/ui/editor/navigation.rs`
  提供树展平和按节点缩进/提升；正文保存仅在章名实际改变时更新同名目录，保留
  独立子目录标题。EPUB 导入的 fragment 当前尚未映射为块目标，编辑器子目录定位到
  所属内容单元，不可按目录标签或序号猜测块位置。
- `web/editor/` 与 `assets/editor/`：固定版本 ProseMirror 源码、lockfile 和提交的构建
  产物；普通 Cargo 构建不运行 Node。
- `src/ui/reader/annotations.rs`、`annotations.js`：Reader 笔记宿主与可信选区菜单，
  闭合 Shadow DOM 隔离正文 CSS，覆盖层绘制而不改写正文节点。
  `src/ui/reader/selection_menu.rs` 是 EPUB 与 PDF 共用的 WebView2 原生「AI解释」菜单，
  由调用方提供私有文档判定与事件构造，仍要求 page/frame 为同一私有文档。
- `src/ui/reader/translations.rs`、`translations.js`：Reader 双语对照展示。宿主在
  `sync_loaded_page` 之后按当前书/单元从 `translations` 表读取与当前
  `(document_revision, unit_revision)` 和对话模型一致的译文，经 session/generation
  单调门控 `evaluate_script` 注入；打开新章后的迟到完成不得覆盖当前章。前端按
  “规范化原文文本（重复文本按文档顺序消歧）”匹配正文块级元素，译文块一律标记
  `data-moye-translation` 并插入原文之前，默认「译文在上、虚线分隔、原文在下」，
  点击译文切换该段的仅译文/双语；表格单元格把译文插到单元格内部且不参与切换。
  译文节点必须从 `annotations.js` 的 `textIndex()`/`currentSelection()` 与
  `READER_INITIALIZATION_SCRIPT` 的 `boundedSelection()` 中排除（选区跨译文时按
  fragment 过滤译文后再取文本），原文文本节点始终保留在 `body`，使笔记 UTF-16 锚点、
  版本校验和重叠标记语义不受翻译影响。译文文本只用 `textContent` 写入，不能当 HTML。
  `src/ui/reader/translations.test.cjs` 是可选 DOM 门禁（Node + 已安装 Playwright），
  覆盖双语顺序、重复文本消歧、嵌套块、单元格插入、点击切换与笔记/选区排除。
- `src/ui/pdf_reader/annotations.rs`、`annotations.js`：PDF 页面笔记宿主与页面桥接，
  复用同一张 `annotations` 表、互斥标记规则、人工/AI 想法流程与展示清洗。锚点作用域
  是单页的 PDF.js 文字层：宿主无法复刻该投影，因此
  `LibraryStore::validate_pdf_annotation_anchor` 不比对规范页面文字，只校验图书/页面
  归属、`ContentUnitKind::Page`、双版本、引文上限，以及范围长度必须等于压缩引文的
  UTF-16 长度；偏移的稳定性来自不可变原件加固定版 PDF.js。
  连续滚动会同时挂载多页文字层，因此文本索引与锚点解析必须按页缓存、按页失效，标记要
  绘制在各自页面坐标上；阅读窗口一次只保留一个文档级 session，页面窗口由前端用
  `pages_rendered` 声明，宿主用自身规范页面解析每页版本与能否写笔记，`list` 可在一次
  往返中取多页笔记。写请求必须指向已声明的规范页且 `revision` 等于该页 `unit_revision`，
  宿主按 session、已渲染页窗口、请求代次与版本拒绝过期请求；`configure` 绑定文档版本，
  `disable` 用于整本文档没有规范身份的情况（同时清空 session 并隐藏笔记控件）。前端不得
  提交 AI 类型，AI 想法只能由宿主在回复保存后写入，且写入发起解释的那一页，不因滚动
  改变目标页。未保存的人工想法或待保存 AI 想法期间，前端 `lockedPage()` 钉住该页：
  宿主 `current_page`、阅读进度与笔记面板不随滚动前进，该页也不会被回收卸载；保存或
  取消后再跟随真实阅读位置。可选 DOM 门禁 `node --test src/ui/pdf_reader/annotations.test.cjs`
  与 `node --test src/ui/pdf_reader/viewer.test.cjs` 使用已安装 Playwright，
  `MOYE_TEST_CHROMIUM` 可指定浏览器；不为测试安装或修改项目依赖。前者用合成多页文字层
  覆盖按页笔记、按页命中与草稿钉页；后者用测试内生成的最小 PDF 驱动提交的 `assets/pdfjs`
  产物，覆盖整本连续滚动成列、远离阅读位置的页面回收与返回重绘、`moye-pdf-page-changed`
  与按页选区上报，以及紧凑阅读的 URL 参数与“切换不跳动”。
  `src/ui/notes.rs` 是“本书笔记”与“全部笔记”共用的原生浏览窗口，查询当前数据库，
  不使用图书窗口的旧投影推断范围；保留单表存储。列表按最近更新排序，搜索、类型筛选和
  分页控制渲染规模，引文按纯文本、人工/AI 想法按安全 Markdown 展示并支持复制。
  Markdown 复用 AI 侧栏的固定版 GFM 清洗；原生总览后台生成按笔记缓存的展示投影，
  表格单元格使用明确列宽与自然换行，宽表格单独横滚；不要使用组件内建表格的 truncate。
  单元格保留安全的行内 Markdown，嵌套表格附完整表格视图，避免长内容被截断。
  WebView 宿主后台生成派生 HTML，前端在 inert template 解析后仅重建白名单元素。
  派生 HTML 上限 8 MiB，超限回退纯文本；不得扩展原始笔记/IPC 的容量上限。
  持久化、搜索、人工编辑与整条复制继续使用原始 Markdown，不存派生 HTML。
  打开章节时重新确认笔记存在及书/单元
  版本，按稳定单元 ID 定位；失效笔记不按引文猜测位置。本书窗口参与删除图书关闭，
  全部窗口可刷新，两个窗口均参与应用退出生命周期。
  2026-09-09 总览验收：隔离两书四章 41 条笔记，真实 Windows 窗口验证全部/本书入口、
  搜索、三类筛选、两页切换、整条复制与跨章打开；本书搜索另一本书返回空结果。
  外部 fixture 新增后刷新保持搜索和类型，总览 41→42、本书 20→21；失效笔记可读且
  禁止跳转。取消退出保留两种笔记窗口和两个 Reader，确认退出全部正常关闭，日志为空。
  Markdown 验收使用独立两书 fixture：真实 Windows 章节/相关面板展示两类想法的标题、
  强调、列表、引用、代码与表格；长代码和宽表格可局部横滚。复制想法与源码文件逐字相等，
  人工编辑显示 Markdown 源码并可保存，选择编辑文本不触发 AI 章节高亮。
  本书与全部笔记真实窗口分别验证 AI/人工 Markdown，长单元格完整换行到末尾标识，
  六列表格横滚后第六列完整可见；两类“复制笔记”均保留完整原始 Markdown。
  点击覆盖层按命中范围的精确 UTF-16 起止和引文筛选人工/AI 想法，不展示纯划线卡；
  命中的划线若没有任何人工/AI 想法（含生成中或待保存的 AI 想法），不打开空列表，而是
  选中该划线的精确范围并显示同一套选区菜单，供改样式、删除划线或写想法；判空必须与
  抽屉实际会渲染的想法卡一致。
  重复文字的其它位置与失效笔记不混入。刷新、编辑和删除保持当前范围，整章入口与页面
  session 变化清除范围筛选。选择菜单的 remove_mark 只接受精确 anchor，不能用想法 ID 删除。
  选区菜单使用 max-content 固定内容宽度与 nowrap，定位前按可见正文视口限制最大宽度；
  极窄区域横向滚动，不能依赖 fixed/auto 的 shrink-to-fit 宽度或把最后按钮折到第二行。
  IPC 校验私有 origin、当前章节、每次页面载入的新 session、版本、请求代次与字节上限；
  前端不得提交 AI 类型。
  Reader 的 selection_changed 只接受 anchor/focus 与 Range 两端均属于当前 document/body
  的非空正文选区；从已校验 Range 读取文字。笔记的闭合 Shadow DOM、输入框及重定向
  到 host 的选区均发送空字符串清除自动引用。focusin 立即清除文本框可能保留的旧选区，
  80ms 回调执行时重新校验，不能捕获旧正文后迟到恢复。手工章节引用与已提交请求不变。
  AI 解释通过独立 Submitted/Completed/Failed 事件绑定请求，只有已保存的最终回复能成为
  AI 想法；失败保留待保存回复供重试或放弃。人工草稿/已接受写入阻止切章关闭。
  可选 DOM 门禁 `node --test src/ui/reader/annotations.test.cjs` 使用已安装 Playwright，
  `MOYE_TEST_CHROMIUM` 可指定浏览器；不为测试安装或修改项目依赖。该门禁同时覆盖命中
  划线时有想法打开相关列表、无想法改为选中划线并显示七按钮菜单两条分支。
  2026-09-09：隔离 EPUB 在真实 Windows 窗口逐项验证四种标记、人工想法保存、未保存
  草稿阻止切章、笔记删除；浮动菜单与原生右键分别通过本地 mock SSE 生成 AI 想法。
  关闭重开后文字标记与人工/AI类型均保留，重复文字的第二处范围没有移到第一处；
  最终关闭产品正常退出、stderr 为空。未连接真实模型或用户书库。
  随后验证点击直线、人工想法与 AI 想法标记只显示各自相关笔记，右下入口切回整章；
  XHTML 鼠标回归覆盖重叠范围、重复文字、刷新/删除保持筛选与章节切换。
  后续修正验收：临时 EPUB 同段含一种标记及人工/AI 想法，真实窗口把马克笔换成波浪线
  后仍只有一条标记；点击“删除划线”后数据库只剩两条想法，相关面板只显示想法卡。
  从长段落行中间跨三行选择时，菜单七按钮保持单行；DOM 回归另覆盖 620/500/320px
  反复选择和缩窗，以及极窄菜单横滚后的 AI 解释点击。
  笔记引用隔离验收：真实窗口选择人工想法、与正文相同的笔记引文及编辑框文字时，AI
  侧栏保持“当前章节”且无自动引用；重新选择正文恢复“当前章节高亮”。HTML/XHTML
  联合桥接回归还覆盖选区复制、80ms 迟到事件及笔记右键清除旧正文解释快照。
- `web/pdf/` 与 `assets/pdfjs/`：固定版本 PDF.js shell、lockfile、清单和提交的本地
  资产；不得改为 CDN 或运行时联网获取。`web/pdf/src/viewer.mjs` 是上下连续滚动的实现：
  页列一次性布局，`IntersectionObserver` 按需绘制、远离阅读位置后回收为占位页；滚动
  停止约 150ms 才上报 `moye-pdf-page-changed`，并复用最近一次 `moye-pdf-go-to` 的
  `requestId` 以保持宿主过期请求拒绝语义。读取 URL 的 `compact=1` 后给 `<html>` 设置
  `data-pdf-compact`：这是“PDF 紧凑阅读”唯一的页间距契约，宿主对已打开窗口也用同一
  属性做实时更新。改动 `web/pdf/src/` 后必须在 `web/pdf` 执行
  `npm ci` 与 `node scripts/build.mjs` 重新生成 `assets/pdfjs/viewer.html`、
  `viewer.mjs` 与 `manifest.json`，并让 `node scripts/check-bundle.mjs` 通过。
- `tests/epub_flow.rs`：运行时生成 EPUB 2/3 fixture 的原有跨模块流程。
- `tests/multi_format_flow.rs`：统一模型、格式、对象存储、编辑、搜索和导出的多格式流程。
- `tests/openai_compatible_flow.rs`：mock OpenAI-compatible models/embeddings/SSE 流程。
- `tests/format_corpus_gate.rs`：仓库外真实格式语料的显式 ignored 门禁。
- `src/bin/gpui_hello.rs`、`src/bin/hello_world.rs`：示例二进制，不是产品入口。

## 工作边界

- 开始前运行 `git status --short`，保留用户已有文件和改动。不要用
  `git reset --hard`、`git checkout --` 等方式恢复不属于自己的内容。
- 不要手工修改 `target/`。不要把本机数据库、对象目录、测试导出文件、外部语料或
  WebView2 用户数据加入仓库。
- 自动测试和人工冒烟不得读写用户真实的 LocalAppData 图书库。测试优先使用运行时
  fixture、`tempfile` 或下文的唯一隔离数据目录。
- 保持 `Cargo.lock` 可复现。普通构建、测试和运行都使用 `--locked`；只有明确增删
  依赖时才更新锁文件，并检查更新范围。
- 不要顺手升级 GPUI、`gpui-component`、Wry/WebView2、`rbook`、`office_oxide`、
  `ebook-rs`、PDF.js 或 `sqlite-vec`。这些依赖已固定并涉及 Windows 句柄、解析安全、
  资产一致性或数据库 ABI；确需升级时按高风险变更重新走完整门禁。
- `Cargo.toml` 中的 Apache-2.0 元数据与当前 `LICENSE` 的禁止封装分发文字不一致。
  在用户解决该冲突前，不要发布、打包、重新分发或替用户作许可证结论。

## 环境与常用命令

需要 Rust stable、`x86_64-pc-windows-msvc`、MSVC C++ Build Tools、Microsoft Edge
WebView2 Runtime，以及 `rustfmt`/`clippy` 组件。仓库没有固定具体 Rust toolchain，
也没有自定义 rustfmt、Clippy 或 CI 配置。

仓库会自动发现三个二进制目标，且没有 `default-run`。运行产品时必须显式选择：

```powershell
cargo run --locked --bin moye-epub-editor
cargo build --locked --bin moye-epub-editor
cargo build --release --locked --bin moye-epub-editor
```

不要使用裸的 `cargo run` 或 `cargo run --release`，否则 Cargo 无法确定要运行哪个
二进制。

常用最终验证命令：

```powershell
cargo fmt --all --check
cargo check --all-targets --locked
cargo test --all-targets --locked --no-fail-fast
cargo clippy --all-targets --locked
```

当前全库可能存在既有 Clippy 告警，因此不要把 `-D warnings` 冒充为已通过的仓库
门禁。修改代码时不得在触及范围内新增告警；若要启用全库 `-D warnings`，先单独
清理并记录基线。

按修改范围快速迭代时可使用：

```powershell
cargo test --lib --locked
cargo test --bin moye-epub-editor --locked
cargo test --test epub_flow --locked
cargo test --test multi_format_flow --locked
cargo test --test openai_compatible_flow --locked
```

`src/ui/` 的单元测试属于产品二进制目标，不包含在 `cargo test --lib` 中。完成 Rust
改动前仍应执行全目标检查和测试；不要只用快速命令作最终验收。

### AI 问答诊断日志

AI 日志统一使用 `moye_ai` target。未设置 `RUST_LOG` 时，默认
`warn,moye_ai=info`，记录问答开始、完成、取消及失败；详细排障使用
`warn,moye_ai=debug`。日志输出到终端，并带源码文件和行号；不会自动写入文件。
需要保留一次复现时，在 PowerShell 中运行：

```powershell
$env:RUST_LOG = "warn,moye_ai=debug"
$aiLog = Join-Path ([System.IO.Path]::GetTempPath()) ("moye-ai-" + [guid]::NewGuid() + ".log")
cargo run --locked --bin moye-epub-editor 2>&1 | Tee-Object -FilePath $aiLog
```

开发验证还须按下文 GUI 冒烟要求设置唯一 `MOYE_DATA_DIR`。环境变量只作用于从该
终端新启动的进程；复现结束后恢复原来的 `RUST_LOG`，或原来未设置时用
`Remove-Item Env:RUST_LOG` 移除。不要用全局 `trace` 抓取 HTTP 请求正文。

- `trace_id` 是进程内跨窗口唯一的问答编号，从请求注册、冻结选区、作用域与历史、
  模型/工具调用到保存和界面完成共用；重启后重新计数。`request_id` 是窗口内代次。
- `http_id` 标识一次 HTTP 调用；`round` 是工具轮次，`attempt` 是该轮启动流的尝试。
  `stage` 与 `error_kind` 区分 HTTP 拒绝、流读取、JSON/工具校验、来源校验和保存失败。
  `AI request token invalidated` 也会在正常释放请求时出现，不代表用户取消。
- HTTP 记录主机、端口、模型、请求字节数、超时、状态码和受控错误类型；流结束记录
  事件数、首事件等待、耗时、`finish_reason` 与 provider 实际返回的 token usage。
  `length` 可作为输出达到限制的排查线索，不能单独证明 JSON 截断的根因。
- 工具仅记录允许的名称、参数字节数、JSON 类型/字段数量、已知数组长度和解析错误
  分类/行/列。未知工具名和 provider 标识归一化；不记录提问、书名、正文、选区、
  回答、完整参数、引用 ID、密钥、HTTP headers 或端点路径/查询串。
- 不逐 token 打日志。新增日志必须使用同一 target、继承异步 span，并经
  `src/ai_diagnostics.rs` 的分类/摘要函数处理不可信值；不能直接格式化任意错误链。
  HTTP/SSE 故障与日志脱敏回归位于 `tests/openai_compatible_flow.rs`。

## Python 课程实验包

`courses/agent-foundations/` 是独立的第一章开发与试学包，使用 Python 3.12、uv 和
LangGraph。它不连接墨页数据库，也不读取真实图书库。包内 README 是学习入口，
`spec.md` 与 `worksheets/rubric.md` 描述课程和评分契约；不得把作品自动检查当作独立
掌握、真实学员试学或专家水平证据。

讲义修订单独标记（当前 `2026-09-07.tutorial-1`），不要为了只改教学支架而变更
实验/评分契约并使已有档案失效；说明中要区分阅读示例、提示后完成与独立重写。
新增教学代码应按当前受控接口运行核对，局部片段注明放入哪个函数/分支、所需导入及
能否直接运行。桌面内嵌讲义使用 `include_str!`，更新文本后需要重新构建产品才能展示。

`courses/ai-agent-tutorial/` 保存完整教程总览和第2—10章正文；示例位于本包
`tutorial_examples/`，复用唯一 `uv.lock`。正文应自足、首次解释新术语、给出输入与
预期结果，外链仅用于延伸阅读。两版示例输出一致只是行为对照，不代表独立掌握。
这些示例是普通本地 Python 程序，不是第一章受控 `run` 提交，也不自动获得桌面隔离。
另有 `exercises/` 中的第2—10章桌面练习说明，提交骨架和参考实现在
`agent-foundations/starters/chNN_*.py` 与 `references/chNN_*.py`；二者不能与本地
无参示例混用。桌面继续使用五参数 `run` 和 Windows LPAC，课程身份通过 `chapter`
绑定；缺省仅表示第一章。后续章节场景为 `normal/fault/transfer`，宿主每次产生虚构
输入；`chapter_runtime.py` 的资料、权限及评分留在宿主，工作进程只得到
`chapter_support.py` 的消息与结果传递帮助函数，不得暂存评分器或参考实现。
第2—10章的一次模型接口调用是获取练习输入，并非实际 LLM 推理；报告与界面需明确
区别。审批、草稿写入与 MCP 为受控模拟，不能据此声称已支持真实外部写入或 MCP 连接。
在本包目录使用 `uv run --locked python -m tutorial_examples` 运行全部章节对照；
`tests/test_tutorial_examples.py` 单独核验权限、证据、记忆和停止等行为。

在课程目录运行以下开发门禁。`uv.lock` 是唯一 Python 依赖锁；普通安装与运行必须
带 `--locked`，只在明确调整依赖时重新生成锁文件。不要变更 Rust 或前端锁文件。

```powershell
Set-Location courses/agent-foundations
uv sync --locked
uv run --locked ruff format --check .
uv run --locked ruff check .
uv run --locked python -m pytest -q
uv run --locked python -m moye_lab compare --scenario all
```

`runs/`、`workspaces/`、`.venv/` 和缓存均忽略，不提交运行报告、个人作答、凭据或
代码快照产物。固定响应模式离线运行；HTTP 测试只使用临时本机 mock 服务。需要真实
模型时显式传 `--mode live --base-url URL --model MODEL`；远程必须另传
`--allow-remote`，远程 HTTP 还必须传 `--allow-insecure`。本实验凭据使用独立的
Windows Credential Manager 命名空间，不复用产品凭据、环境变量或明文配置文件。
不得自动启动 Ollama 或下载模型。

课程代码通过 `run(task, model, tools, limits, emit)` 注入。宿主观察模型和工具调用，
核验回填、来源、重试与预算；学生 `emit` 和框架节点标注只是展示信息。CLI 的进程内
运行器仍只执行已审查代码，不能把它当作隔离执行器。

桌面从课程 `.venv/Scripts/python.exe -I -u moye_lab/desktop_host.py` 启动受信宿主，
宿主通过受限 RPC 执行一次性 Windows LPAC 工作进程，不在宿主导入学生代码。每次
复制独立解释器、依赖与课程接口，课程文件仅从暂存区读取；模型、资料、工具校验和评分
留在宿主。固定允许 `registryRead` 以初始化系统 DLL，不授予网络能力；启动前核验
AppContainer SID、能力集合、Job 和实际 AAP 文件授权差异，普通 AppContainer 必须
被拒绝。只封闭本次临时 profile 的文件和注册表写入，不修改系统或用户原有 ACL。
Job 限制单进程、256 MiB、10 CPU 秒及 25% CPU；应用另限制 180 秒课程期限、输出和
RPC 数量。取消/宿主异常退出必须终止 Job 并清理 profile，不得回退非隔离执行。

本章边界是同步 `run`。工作进程内的 `desktop_compat.py` 延迟 CPython 3.12 的真实
`_overlapped` 初始化，让同步 LangGraph 能导入 asyncio；不模拟 IOCP 功能，实际访问
原生 IOCP 和网络仍受系统拒绝。不修改已安装解释器或第三方依赖，兼容标识与源码
进入报告。不要把这个适配解释为已支持任意异步或联网实验。

`tests/test_sandbox_windows.py`、`test_sandbox.py`、`test_desktop_host.py` 验证真实
Windows 隔离、资源耗尽、伪造报告、管道背压、取消及宿主死亡。

第2—10章的宿主规则与变式由 `test_chapter_runtime.py` 验证，真实隔离矩阵与反例在
`test_chapter_sandbox.py`（九章 × 三场景 × 两实现）。资料、请求、样本等 ID 不得编码
判断类别，洗牌也不能代替内容变式。大批 LPAC 运行复制临时解释器，pytest 历史目录
清理可能拖慢收尾；需要指定 `--basetemp` 时只用本次唯一临时路径，不能指向已有目录。

桌面跨语言专项门禁：

```powershell
cargo test --lib --locked learning::tests::supervisor_control_flow -- --ignored --nocapture
cargo test --lib --locked learning::tests::desktop_learning_runs_both_implementations_and_restores_evidence -- --ignored --nocapture
cargo test --lib --locked learning::tests::desktop_later_chapters_preserve_identity_and_restore_only_their_own_evidence -- --ignored --nocapture
```

学习档案采用版本、摘要、容量限制、revision 校验和临时文件原子替换；损坏档案只读
报错，允许验证备份后原样归档坏文件再恢复。导入报告必须显示待核验，文件摘要只
证明一致性，不证明实际运行或掌握。每轮最多 64 次报告、总档案最多 32 MiB；新一轮
先归档历史再保存当前编辑，不能先向已满档案追加。关闭学习窗口需等待取消、已接受
保存及报告落盘；失败时保留窗口和可重试提示。

只修改此独立 Python 包与文档时运行上述 Python 门禁；若触及 Rust 或桌面路径，
仍须执行本文件规定的 Rust 全目标验证和相应 GUI 路径。

## 前端构建产物

只有修改 `web/editor/`、`web/pdf/` 或对应依赖时才需要 Node.js 22+。前端统一使用
`npm`；两个目录各自的 `package-lock.json` 是唯一权威依赖锁，必须与 `package.json`
和确定性构建产物一起提交。不要使用或生成 `pnpm-lock.yaml`、`yarn.lock` 等第二套
锁文件。最终 Rust 验证前检查构建产物没有陈旧：

```powershell
Push-Location web/editor
npm ci
npm run check
npm run build
npm run check
Pop-Location

Push-Location web/pdf
npm ci
npm run check
npm run build
npm run check
Pop-Location
```

`web/editor` 的 `check` 同时执行 TypeScript 类型检查和 bundle 一致性检查；
`web/pdf` 的 `check` 校验 PDF.js 版本、清单摘要、CSP、本地资源与禁用 eval/XFA 等
安全契约。不要手工编辑生成的 `assets/pdfjs/routes.rs`、manifest 或构建 bundle。

## 真实格式语料门禁

真实第三方语料不加入仓库。将以下九个文件放入一个仓库外目录，且只使用有权测试的
DRM-free/非加密样本：`sample.epub`、`sample.pdf`、`sample.doc`、`sample.docx`、
`sample.pptx`、`sample.xlsx`、`sample.mobi`、`sample.azw`、`sample.azw3`。

```powershell
$env:MOYE_FORMAT_CORPUS = "C:\path\to\moye-format-corpus"
cargo test --test format_corpus_gate --locked -- --ignored --nocapture
Remove-Item Env:MOYE_FORMAT_CORPUS
```

该门禁对每种格式验证魔数/容器识别、统一模型、稳定 locator、规范化编辑、FTS 搜索、
EPUB/PDF/原件导出、重新打开及原件字节一致性。
升级 `office_oxide`、`ebook-rs`、`lopdf` 或格式探测规则时，除生成 fixture 外还必须
运行此门禁，并补充复杂、损坏、加密、超大与压缩炸弹样本的预期结果。

## 编码与架构约定

- 遵循标准 `rustfmt` 和现有 Rust 命名风格。用户可见文案保持中文并沿用现有错误
  风格；诊断使用 `tracing`，不要在产品路径新增 `println!`。
- 使用 `anyhow::Context` 为文件、数据库、格式解析、模型请求和 WebView 错误补充可
  操作上下文；不要静默吞掉会影响数据或用户流程的错误。
- 耗时的哈希、归档/文档解析、对象 I/O、索引、数据库批量操作、模型调用、Office
  COM 和导出不得阻塞 GPUI。使用 `AppServices` 的独立 runtime/后台任务，并在正确
  的 GPUI context 中更新实体。
- 事务已提交的视觉任务按完整 `VisualJobSpec` 确认调度；后台可能已把它推进到
  Running / Succeeded，不得仅因不再 Queued 就误报提交失败。缺失、身份变化、停止
  状态或队列关闭仍须报错，迟到的唤醒不得重置任务或重复渲染。
- GPUI 状态改变后沿用 `cx.notify()`；事件 `Subscription` 必须保存在实体字段中，
  避免订阅因临时值析构而失效。关闭窗口时取消流式请求和后台回调。
- 每个顶层 UI 的结构体、`Render`、私有状态和专用 WebView/IPC helper 放在
  `src/ui/` 对应单个文件；跨窗口主题和安全关闭基础设施才进入 `ui/mod.rs`。
- 修改功能时补最接近实现位置的单元测试；跨导入、持久化、阅读、搜索与导出边界的
  行为补到相应集成测试。测试使用临时目录和生成 fixture，不依赖个人文件。
- 平台专用行为使用明确的 `#[cfg(target_os = "windows")]`。不要为了消除 Windows
  特有分支而削弱已验证的 HWND、WebView2、Credential Manager 或 Office STA 处理。
- 当前开发期不迁移：结构或数据关系不一致时重建数据库与受管对象目录

## 统一模型与格式约束

- `BookDocument` 是导入器、编辑器、导出器、搜索与 AI 引用共享的事实模型；稳定 ID、
  revision、内容单元顺序、独立 TOC 和 `DocumentLocator` 语义不得由 UI 临时推断。
- 章节正文只使用 HTML，领域模型、编辑接口和 `content_units` 不再保存或选择
  Markdown 格式。源码只有成功解析、清洗并生成 AST 后才能保存；块编辑可以规范化
  HTML 标签和空白。聊天回复与课程讲义的 Markdown 展示独立于图书章节格式。
  Office 使用解析器的原生 HTML 输出，PDF 文本序列化为 HTML；Office 章节划分依据
  规范化标题块，不能按正文中的 `#` 猜测标题。列表、引用、代码块和普通表格保留结构，
  统一模型不能表达的复杂 HTML 保留为清洗后的 RawHtml，不能静默丢弃内容。
- 所有导入都永久保留字节一致的原件；只有导入图书提供原件导出。Office/Kindle 编辑
  结果不得伪装成能回写原格式；统一模型只保证可规范化导出 EPUB/PDF，不保证复刻原
  Office、PDF 或 Kindle 的复杂版式。
- 新格式必须实现窄 `DocumentImporter::probe/import` 边界，以内容签名优先于扩展名，
  并应用资源限额。不要让解析器对象、磁盘路径或对象键进入领域模型。
- Office 基础预览来自统一模型。COM 增强只对用户确认可信的文件按书显式触发，在专用
  STA 工作者中只读打开原件，禁用宏与外链更新且不主动调用宏/OLE，只把临时 PDF/图片
  转换为受管视觉页；不得把 Office 自身解析嵌入内容描述为安全沙箱。增强状态和页面
  持久化，但页面仍是可重建派生数据；失败、取消、禁用或未安装 Office 时继续使用
  结构化预览。

## 数据与对象一致性约束

- 产品运行路径的 SQL 只放在 `src/db/`。单表 CRUD 和查询行直接映射结构体放在与表
  同名的文件，表模块接收现有连接，不自行开连接或提交；跨表原子操作只放
  `transactions.rs`。仅用于 fixture、故障注入或断言的短小 SQL 可以放在相邻
  `#[cfg(test)]` 测试中，但不得借此在产品模块另建可复用查询路径。
- 当前处于开发阶段，不维护数据库向前/向后兼容，也不编写迁移、旧字段回退或遗留
  JSON 兼容。结构变化必须递增 `schema::SCHEMA_VERSION`；启动发现应用标识、结构
  版本、表/索引、SQLite 完整性或数据关系不一致时，删除 `library.db` 及 WAL/SHM/
  journal 和受管 `objects/`，再创建空库。结构判定必须覆盖规范 DDL 中的约束、外键、
  默认值、触发器正文及意外遗留对象，不能只比较列名。若对象目录清理失败，必须移除
  本次刚创建的空数据库，使下次启动仍会重试完整重建。凡同时保存 `book_id` 和
  `source_id` 的表，都必须验证 source 确实属于该书；不能把“两个外键各自存在”当作
  跨表关系有效。FTS 完整性检查应使用行数与双向集合差异检测，避免按 UNINDEXED ID
  对每个 chunk 做相关扫描。
- 锁冲突、权限不足、只读、磁盘已满或普通 I/O 错误不是可删库条件，必须原样报错；
  不要用重建逻辑掩盖它们。
- SQLite 保存正文/AST、目录、元数据、引用、任务与索引；原件、封面、媒体和派生页面
  只存为不可变 BLAKE3 对象。对象键和文件路径不得泄漏到领域层。
- 新引用先写不可变对象，再在一个 SQLite 事务中发布图书图；失败对象由启动 GC 回收。
  删除先以事务解除引用，再异步删除对象，失败留待 GC 重试。
- 图书修改、导入、删除、正文和 FTS 更新保持事务语义；embedding、vision 与视觉页面
  是按模型/内容/renderer/revision 可重建的派生数据，不得使 FTS 或正文提交失败。
- 媒体自定义协议必须校验 book/asset 归属、单 Range、`206`/`Content-Range`、MIME 和
  大小边界；不要把本地对象路径暴露给 WebView。
- `LibraryStore` clone 可能持有旧 UI 投影。跨窗口读取使用 `spawn_library_read`，写入
  使用 `AppServices` 中的唯一共享实例；阅读进度也不得从窗口 clone 直接落库。会更新
  可见书库状态的写操作使用 `spawn_library_projected` 返回同一锁区间内生成的单调版本
  快照；mutation 在 API 边界取得 FIFO ticket，不能依赖 `spawn_blocking` 调度顺序代表
  用户请求顺序。窗口只合并不旧于已应用版本的投影，并保留更高 revision 的编辑结果。
  不得在 GPUI 回调里同步等待共享 mutex，也不得让延迟快照覆盖较新的分组、进度或正文
  状态。跨 Reader/PDF Reader 窗口的进度动作使用进程级递增序列，并保存稳定的
  content-unit locator；不要把旧窗口 ordinal 直接绑定到当前 revision。
- 同一数据目录只支持一个应用进程；上述 FIFO、递增序列和投影代数只协调一个
  `AppServices` 内的多窗口，不提供跨进程排序或单实例互斥。测试或人工运行不得同时让
  两个产品进程指向同一隔离目录，除非任务就是实现并验证跨进程协调。
- 导出先在目标同目录写入并同步临时文件，再原子替换；替换是提交点，提交前失败不得
  破坏已有文件或留下临时文件，提交后的目录元数据强化同步失败只能记录告警，不能在
  新目标已经可见时向调用方谎报“导出失败”。

## 搜索、视觉与 AI 安全约束

- 标题、正文、表格、图片说明/OCR 和视觉说明共享稳定 chunk/locator。视觉识别区域用
  `DocumentLocator` 的 0..=1000 归一化正面积矩形表达，模型输出的数量、坐标、重复项
  和文本总量都必须受限；同一页面的全部区域分块必须原子替换。FTS5 和向量分别召回，
  混合模式用 RRF；向量不可用必须降级为作用域内 FTS。
- `VectorIndex` 必须在排名前应用 `allowed_book_ids`，`SearchService` 加载结果时再次
  硬过滤。不要依赖 UI、prompt 或模型自觉维护权限。
- embedding、visual render、vision 任务状态和 cursor 持久化到 SQLite，支持重启
  恢复及暂停/恢复/重试/取消。视觉渲染以逐页 staging 和同事务 cursor 形成连续断点；
  恢复不得重复光栅化已持久页面，只有完整页面集、staging 清理和 `Succeeded` 状态能在
  同一事务提交后替换旧页面。内容、模型或 renderer 变化只重建派生数据。
- Word/Excel 整本 PDF 增强页（即使页数与单元数相同）也没有页面与统一内容单元精确
  对应的证据，必须使用 `OfficeRenderedPage`、`content_unit_id = NULL` 和明确的
  preview-only identity，只能用于预览；不能按页数、比例或顺序猜测归属。vision worker
  必须在解包内容单元前识别这类页面，清除同页可能遗留的视觉 chunk、推进 cursor，且
  绝不调用视觉模型。FTS/KNN、SearchService、Agent 清洗和引用提交事务均须再次拒绝
  此类 locator。PPTX 独立逐页图片路径必须保留精确的幻灯片 locator 与内容单元归属。
- OpenAI-compatible 默认端点固定为本机 Ollama，不得静默切云。非回环端点发送内容
  前要求确认，非 HTTPS 远程端点还要单独允许；密钥只进 Windows Credential Manager。
- 多 Endpoint 的附加端点、名称和三类模型绑定存于独立的
  `ai.openai_compatible.endpoint_routing.v1` settings key，与 Provider、对话参数和后台
  偏好在同一事务提交。默认端点继续使用 Provider 字段，不增加旧字段回退；端点 ID
  唯一，规范化 URL 不得重复，模型绑定必须存在。每个端点独立校验远程授权和超时，
  URL 改变时清除授权及密钥草稿；密钥按规范化 URL 隔离，保存失败需回滚所有已修改密钥。
  问答注册时在同一锁内固定 Provider、模型、参数与 SearchService；查询 Embedding、
  后台 Embedding 和视觉请求分别使用自己的端点，修改对话端点不得重建派生索引。
  语义 KNN 在同一 SQL 读取中校验源任务的 Embedding 执行身份；旧请求快照不得将
  旧端点的查询向量匹配到新端点的同名模型索引，身份不符时降级为作用域内 FTS。
  `tests/multi_endpoint_flow.rs` 使用多个本机 mock 服务验证重启、三类路由、密钥隔离、
  实际后台任务和问答准备期间切换端点。
- 对话生成参数与 Provider 配置一并校验和保存，使用独立 settings key 并在同一事务
  提交，不扩展旧 Provider JSON 契约或增加旧字段回退。Temperature 可空且范围为
  0..=2（默认 0.1），Top P 可空且范围为 0..=1，Presence/Frequency penalty 可空且
  范围为 -2..=2；浮点值必须有限。空值省略对应请求字段，最大输出 token 必须为
  正整数（默认 4096），不设模型业务上限，由用户按所选模型设置；内部使用 `u32`
  表示并拒绝整数溢出，不能作为服务端上下文容量设置。新问答固定参数快照，各工具
  轮次及联网后备回答沿用该快照；保存设置不影响在途请求，不改变 embedding/vision。
  上下文缩减只能降低输出上限；不完整工具 JSON 的既有恢复请求仍使用温度 0。
  参数区的“恢复默认”只重置参数草稿，不提交设置，也不改其它草稿。覆盖参数校验、保存后
  重启、可空字段序列化、工具/联网轮次和恢复优先级的 mock 回归；GUI 验证需使用隔离目录。
- 后台任务自动运行配置使用独立 settings key，默认关闭（`DEFAULT_AUTO_RUN_BACKGROUND_JOBS`
  = false，AI 设置中“自动运行新创建的后台任务”默认不勾选），不得扩展 Provider JSON。
  默认或关闭时，新导入、创建或保存编辑仍须在文档事务内创建 `visual_render`、`vision`、
  `embedding` 三类任务，但初始状态为 `Paused` 且未开始；已有任务不随设置切换改变状态，
  用户仍可在后台任务窗口逐项恢复。
- PDF 紧凑阅读使用独立 settings key（`pdf.reader.preferences.v1`，默认关闭，AI 设置
  “系统配置”中的“PDF 紧凑阅读”默认不勾选），同样不得扩展 Provider JSON；新 key 必须
  加入 `AI_SETTINGS_KEYS`，否则 `snapshot_ai_settings`/`restore_ai_settings` 的失败回滚
  会不对称（`restore_ai_settings` 校验备份行数）。页间距只有一份契约：宿主用
  `moyepdf://viewer/viewer.html?...&compact=1` 让新窗口在首帧前设置
  `<html data-pdf-compact="1">`，保存设置后再用一次 `evaluate_script` 切换同一属性，
  因此已打开的阅读窗口无需重开即可跟随；两种通道必须幂等，前端不要为该偏好新增 API。
  该偏好作用于所有 PDF 阅读窗口（含 Office 增强预览）：`src/ui/mod.rs` 的
  `PdfReaderWindowRegistry` 是唯一枚举它们的登记表，登记与广播都要顺带剪掉失效 weak，
  不得只依赖按书的单例窗口表。只改页间距（`margin-bottom`），不改变页面几何、投影、
  笔记坐标与滚动位置。切换时必须保持阅读位置：前端在 `MutationObserver` 里用
  “临时还原属性 → 量旧布局 → 恢复属性 → 量新布局”得到精确位移，只补偿浏览器锚定之后
  仍存在的差额（<0.5px 不动手），并用 `takeRecords()` 丢弃自己的两条记录；
  锚点页必须取“视口中心覆盖的那一页”（草稿钉住的页优先），不能用 `currentPage`——
  它只跟踪已绘制的页，刚滚到、仍在占位状态的页会让它滞后一页，补偿就会少算一个页间距。
- 默认显示语言使用独立 settings key（`translation.preferences.v1`，默认关闭，AI 设置
  “系统配置”中的下拉默认选“不翻译（仅原文）”，预设中/英/日/韩/法/德/西/俄等），同样
  不得扩展 Provider JSON，且必须加入 `AI_SETTINGS_KEYS`。`ProviderSettings::validate`
  只接受 `TRANSLATION_LANGUAGES` 中的标签，UI 与校验共用这份常量。目标语言变化或对话
  模型/端点变化由 `configure_translation` + `reconfigure_translation_jobs` 重排翻译任务；
  关闭翻译只取消任务，不删除已存译文（改回同一语言可立即复用）。源语言与目标语言按
  主语言子标签比较后跳过整本翻译。
- Agent 只允许 `search_books`、`read_passages`、`get_outline` 三个只读工具。宿主先
  计算授权 book IDs，模型参数只能缩小范围；保留工具轮次、结果数、上下文和超时限制。
  `AgentLimits::max_tool_rounds` 的既有计数单位是实际工具调用（默认 6 次），不是模型
  回复轮次；一次回复可以包含多个调用。runtime 必须以 `remaining_tool_calls()` 决定
  是否继续提供工具。同批超额调用只回填固定 `tool_budget_exhausted` JSON，保持每个
  assistant/tool ID 配对，不执行、不登记来源、不发送已执行事件；之后只有一次无工具
  最终回答。收尾指令追加到首条 system，不能破坏历史裁剪索引。权限、超时、上下文和
  来源错误仍按原路径拒绝，不能把任意 `agent_limit` 转成正常回答。HTTP 回归使用多调用
  SSE 验证预算、未执行结果、来源与 Reader 会话落盘。
- 上下文超限仅在 provider 明确拒绝启动流时最多重试三次；先按完整轮次减少旧历史，
  再按完整记录减少搜索/片段结果，并从引用注册表去除已不在当前请求里的 passage
  marker。字节比例仅辅助估计裁剪量，不能当作精确 token 数。保留当前问题、冻结
  选区、系统策略、工具定义和 assistant/tool 配对；每个工具结果至少留一条原文。
  SSE 开始后不重试，取消沿用原请求令牌，不自动提高模型服务端上下文上限。
- Ollama 明确返回 HTTP 500 且错误封套报告已提供工具的参数为不完整 JSON 时，每次
  问答最多追加一次宿主固定提示并重试流启动；不得把服务端错误原文注入 prompt，
  不得猜补参数、重跑已完成工具或降低来源校验。普通 500、未提供工具和流内错误不重试；
  该次数不得重置上下文重试计数，取消仍需阻止下一次请求。
- 冻结选区只有在宿主重新校验 scope、book/unit/locator 和当前 document/unit revision
  并签发稳定 citation ID 后才能作为来源。最终回答必须引用本轮已登记来源，或使用宿主
  协议明确表示无来源；模型生成、正文夹带或被篡改的 citation ID 一律拒绝。
- AI 请求必须在任何异步选区冻结前注册取消令牌，并让冻结、宿主授权、provider 调用和
  持久化共享同一请求代次；取消或窗口关闭后不得重新注册同一请求继续执行。
- SSE 正文增量只是临时显示；tool call、取消、流错误和来源校验失败必须撤回临时正文，
  只有宿主完成来源协议校验并发出 commit 后才能持久化最终回答。
- Library 会话范围来自当前分组及全部后代；Reader/Editor 范围必须包含当前书，可由
  用户追加其它书。编辑器未保存引用必须经过 session/chapter/revision/request-id 精确
  快照冻结，不能写入持久搜索索引，也不能使用过期 Rust 缓存。
- 对话按窗口独立持久化。引用必须保存可验证 locator、document revision 和 unit
  revision；scope、当前来源、块/文本范围及搜索 chunk 一致性必须与消息在同一 SQLite
  事务中校验。恢复和点击旧引用时应明确标记 stale，不能降级跳到同一单元的其它页面。
  从历史会话恢复的未保存 selection 还必须按 block/text range 与当前 AST 精确匹配，
  或在对应 unit/block 中唯一匹配原 quote；无法证明时必须 stale。视觉区域 passage 不得
  伪装成 selection，也不得用可重排 AST 文本复验。
  Reader/Editor 点击正文引用时还要重新解析当前 AST 并在目标页面唯一匹配文字范围；
  缺失、歧义或身份不一致都必须拒绝跳转，不能退化为章节开头。
  模型返回的书 ID、引用或工具参数都视为不可信输入，提示注入不得扩大权限或启用写操作。
  Reader 的“AI解释”通过 Windows 原生 WebView2 菜单采集非编辑区域的有界选区，
  要求 PageUri 与 FrameUri 为同一私有阅读文档（子 frame 由 CSP 禁止）；不能单凭
  IsRequestedForMainFrame 判定，实际子 WebView 会对当前阅读文档返回 false。
  菜单回调仅入队，不同步借用 GPUI；源 URL 必须匹配当前章节。新会话初始化成功后才发送
  固定提问，使用菜单打开时的选区，不携带旧会话历史或其它勾选引用；关闭后丢弃迟到回调。

## 不可破坏的 WebView 与窗口约束

1. `src/main.rs` 必须在 GPUI 启动任何工作线程前设置
   `GPUI_DISABLE_DIRECT_COMPOSITION=1`，并在创建应用前完成 WebView2 Runtime 探测。
   应用启动时调用 `gpui_component::init(cx)`；每个使用 `gpui-component` 的窗口必须以
   `Root::new(...)` 作为第一层视图。
2. Reader、Editor 和 PDF Reader 的 Wry 子 WebView 必须通过
   `build_as_child_async(...).await` 异步创建。等待过程位于 GPUI entity/state 借用
   区间之外，完成后再重新进入 context 附加实体，避免 Windows 消息泵重入借用 panic。
3. 捕获的 `Win32WindowHandle` 只是父 HWND 的借用值。异步构建尚未结束时，必须通过
   现有 build gate 否决关闭并延迟处理；不要假设 detached task 随窗口自动取消。
4. Reader/Editor/PDF Reader 关闭时，先从状态和渲染帧移除并隐藏子 WebView，等待
   entity 释放，再调用现有原生窗口移除 helper。不要让默认 `WM_CLOSE` 抢先销毁父 HWND。
  Editor 关闭先询问保存并关闭、不保存或取消；只有确认保存才进入快照和写入流程，
   放弃修改直接安全释放，取消保留输入与 AI 状态。初始化或已接受写入尚未完成时先
   等待；确认框期间若旧请求开始写入，放弃关闭也须等其结果回填，不得另行触发保存。
   Reader/PDF Reader 还必须等最终稳定 locator 的阅读进度写入成功；写入失败时保持窗口
   打开并允许再次关闭重试，不能把“后台 worker 已退出”当作持久化成功。
5. 图书库主窗口关闭先询问“退出软件 / 继续运行”；取消前不得最小化、取消 AI 或
   改变窗口关闭状态。确认后通过 `on_window_close` 注册的原关闭回调逐个关闭子窗口，
   编辑取消、快照/保存/阅读进度/学习持久化失败须调用 `cancel_application_exit` 撤销
   本次退出。AI 设置已经开始保存时等待结果。子窗全部移除后，主窗口再等待已接受的
   导入、创建、分组、移动或删除 mutation 完成，最后安全移除并让 GPUI 自然退出。
   不调用 `App::quit()`/`cx.quit()` 绕过 close veto；`on_window_closed` 订阅保存在全局
   协调器中并 defer 推进。实际开窗前检查退出状态，防止迟到的异步打开留下孤立窗口。
6. EPUB 后续目录、上下章和搜索跳转在 `load_url` 前必须经过
   `OpenedBook::navigation_url_for_href`；Windows 不会自动复用 builder 阶段协议映射。
7. Windows Debug 主线程只有约 1 MiB 栈。Editor 这类大型 GPUI `Render` 不得重新
   合并为单个巨型 builder 表达式；较大的区域应通过非内联 helper 单独构造并在边界
   type-erase，否则编译器可能为一次渲染预留数百 KiB 栈帧并在首帧溢出。
8. Wry 的 initialization script 在 XHTML 根元素建立前执行。ProseMirror bundle 的
   第三方模块会在求值阶段访问 `document.documentElement`，所以必须保留构建脚本中
   对整个 bundle 的 `DOMContentLoaded` 外层门禁；只在 TypeScript 入口函数里等待
   DOM 仍然太晚。修改后同时运行前端 typecheck 和 bundle 一致性检查。
9. `gpui-component 0.5.1` 的 `overflow_x_scrollbar()` 会把原元素样式转移到
   `size_full()` 外壳并清空内部布局。固定高度的横向工具栏使用带稳定 ID 的 GPUI
   `overflow_x_scroll()`，不要把该包装器直接套在承载按钮的 `h_flex()` 上。
10. 富文本 IPC 返回完整的可信 XHTML 页面壳，但只有 `<body>` 属于可编辑正文。更新
    统一 AST 前先提取该元素再清洗解析；不要把含 `<head><title>` 的整页交给片段
    清洗器，否则移除标签后留下的标题文本可能进入正文并在保存时重复。
11. HTML 源码不是 XHTML。编辑器首次投影及源码刷新都用 `markup::serialize_xhtml`
    从已清洗的 AST 生成展示片段，再放入 XHTML 壳；通过 HTML DOM 处理空元素、
    布尔属性与实体并按 XML 转义，不能只替换 `<img>` 或放宽前端严格解析。
    不为展示改写数据库源码，也不能再次清洗并误删受控任务勾选框或媒体。
    富文本 `body: null` 回执与已接纳章节一致时，只释放精确匹配的待处理动作，不重建
    AST；其附带的是展示壳，重新解析会把正文变成 RawHtml 并丢失结构化媒体引用。
    若前次正文解析失败，回执仍携带未接纳正文，必须重试校验并继续阻止保存和切章，
    直到正文修复；不能把前端的 unchanged 标记当作宿主已经接纳该快照的证据。

## 不可破坏的内容安全约束

- EPUB Reader 只提供 manifest 声明资源；保留编码路径穿越、非法路径/MIME 拒绝、
  内部 origin 白名单、CSP、`nosniff`、禁脚本/联网/外部导航/新窗口/下载和 incognito。
- PDF.js 必须完全本地加载，禁止 PDF JavaScript/XFA 和网络请求；协议路由只接受固定
  资产与当前 PDF，不得映射任意主机文件。
- Editor 的宿主初始化脚本是可信桥接，不代表允许书内脚本。IPC 必须校验 origin、
  href、session/revision/request-id、Ready/请求状态和大小上限。保存、切章、导出、
  确认保存后关闭和 AI 引用必须取得完全匹配的快照，旧页面回调不能覆盖新章节。
- 打开 Editor 时只从当前 revision 的 `BookDocument` AST 生成会话投影，不读取或重新
  打包整本原格式容器，也不回退提供原件中的 CSS/字体/任意路径资源。持久媒体只在
  协议请求时经 `AppServices` 后台读取；关闭时先封闭协议响应屏障，再释放 WebView，
  禁止迟到任务向已销毁的 Wry responder 回调。
- 富文本 shell 与不可信正文保持 origin/能力隔离。HTML 中仅允许白名单音视频元素；
  RawHtml 必须清洗，外链、事件处理器和主动嵌入不得进入预览。
- 不要随意提高归档、正文、IPC、视觉页面或模型上下文大小上限。调整时补上限内、
  越界、取消与资源耗尽测试并说明风险。

## GUI 冒烟与完成标准

Debug 构建支持 `MOYE_DATA_DIR`；Release 构建忽略它并访问真实 LocalAppData。人工
GUI 验证必须使用唯一隔离目录。

全目标测试会因 dev-dependency 合并 GPUI 的 `test-support` 特性；不要直接用测试
命令留下的产品 EXE 作最终 GUI 验收。先单独执行产品构建
`cargo build --locked --bin moye-epub-editor` 或下方的产品 `cargo run`。
GPUI 0.2.2 的测试执行器在真实 Windows
dispatcher 上会直接拒绝带超时的等待，从而在退出时产生
`timed out waiting on app_will_quit`；不能未经正常产品构建复测就将它认定为应用退出故障。

```powershell
$env:MOYE_DATA_DIR = Join-Path ([System.IO.Path]::GetTempPath()) ("moye-agent-" + [guid]::NewGuid())
$env:RUST_LOG = "error"
cargo run --locked --bin moye-epub-editor
```

涉及 UI、WebView、导航、编辑器、Office 或窗口生命周期时，除自动验证外还要实际
走完受影响路径。按范围覆盖：启动；导入 EPUB/PDF/Office/DRM-free Kindle；结构化和
PDF 预览；新建图书；HTML、目录、封面和媒体编辑；保存；三种搜索模式；
AI 侧栏与引用；原件/EPUB/PDF 导出并重新打开；多 Reader/Editor 窗口；WebView 构建中
与构建后关闭。

Office 相关改动要在安装和未安装 Office 的环境分别验证，并覆盖超时/取消/回退。
AI 相关改动优先以 mock OpenAI-compatible 服务验证 SSE、工具、范围和取消；真实
Ollama 冒烟不得自动启动服务或下载模型。媒体改动要覆盖 seek/Range 和缺失资产。

关闭图书库时验证继续运行保留全部窗口、确认退出关闭全部窗口，以及编辑器取消/保存
失败中止退出、再次关闭可重试；确认保存退出后重新打开应保留保存内容，且
`RUST_LOG=error` 不应出现 HWND/WebView 错误。在非 100% DPI 下自动化时区分截图逻辑
坐标与输入物理坐标，优先使用控件命中或 DPI 感知坐标。

没有实际执行 GUI 路径时，应明确写成“未做人工 GUI 验证”，不能仅凭编译或单元测试
宣称运行时问题已经解决。交付前报告实际命令、结果和未验证边界；Rust 改动至少通过
格式检查、全目标编译和全目标测试，运行时界面改动还需完成与风险相称的 GUI 验证。
