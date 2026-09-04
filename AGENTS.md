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
- `src/document.rs`：稳定 ID 的格式无关模型，包括 `BookDocument`、`ContentUnit`、
  `BlockDocument`、`TocNode` 和 `DocumentLocator`。
- `src/formats/`：`DocumentImporter` 注册表及 EPUB、PDF、Office、Kindle 适配器；
  `office_oxide`、`ebook-rs` 等第三方类型必须在本目录内转换为统一模型。
- `src/markup.rs`、`src/editing.rs`、`src/export.rs`：Markdown/HTML 解析清洗、事务式
  模型编辑，以及原件/EPUB/PDF 稳定导出。
- `src/storage.rs`、`src/media.rs`：应用自有 `BlobStore`、基于 `object_store` 的本地
  BLAKE3 内容寻址实现，以及带图书归属校验和 Range 支持的媒体响应。
- `src/library.rs`：SQLite 与对象存储之上的图书库业务编排、导入/创建/保存/删除、
  垃圾回收和兼容现有 UI 的投影。
- `src/db/`：SQLite 连接、当前结构、单表 CRUD/查询映射和跨表事务。每张表对应一个
  文件：`books.rs`、`book_sources.rs`、`content_units.rs`、`toc_entries.rs`、
  `blobs.rs`、`assets.rs`、`asset_refs.rs`、`progress.rs`、`search_chunks.rs`、
  `embeddings.rs`、`index_jobs.rs`、`visual_pages.rs`、`visual_page_staging.rs`、
  `chat_threads.rs`、`chat_messages.rs`、`chat_citations.rs`、`groups.rs`、`settings.rs` 和
  `office_enhancements.rs`；跨表原子操作只放 `transactions.rs`，FTS 查询放
  `book_search.rs`，建表与完整性契约放 `schema.rs`。
- `src/search.rs`、`src/indexing.rs`：作用域内 FTS5/`sqlite-vec` 精确 KNN、RRF 混合
  召回，以及可恢复的 embedding/vision 后台任务。
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
  的派生任务；`mod.rs` 只保留跨窗口主题、窗口打开与安全关闭基础设施。
- `web/editor/` 与 `assets/editor/`：固定版本 ProseMirror 源码、lockfile 和提交的构建
  产物；普通 Cargo 构建不运行 Node。
- `web/pdf/` 与 `assets/pdfjs/`：固定版本 PDF.js shell、lockfile、清单和提交的本地
  资产；不得改为 CDN 或运行时联网获取。
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
- GPUI 状态改变后沿用 `cx.notify()`；事件 `Subscription` 必须保存在实体字段中，
  避免订阅因临时值析构而失效。关闭窗口时取消流式请求和后台回调。
- 每个顶层 UI 的结构体、`Render`、私有状态和专用 WebView/IPC helper 放在
  `src/ui/` 对应单个文件；跨窗口主题和安全关闭基础设施才进入 `ui/mod.rs`。
- 修改功能时补最接近实现位置的单元测试；跨导入、持久化、阅读、搜索与导出边界的
  行为补到相应集成测试。测试使用临时目录和生成 fixture，不依赖个人文件。
- 平台专用行为使用明确的 `#[cfg(target_os = "windows")]`。不要为了消除 Windows
  特有分支而削弱已验证的 HWND、WebView2、Credential Manager 或 Office STA 处理。

## 统一模型与格式约束

- `BookDocument` 是导入器、编辑器、导出器、搜索与 AI 引用共享的事实模型；稳定 ID、
  revision、内容单元顺序、独立 TOC 和 `DocumentLocator` 语义不得由 UI 临时推断。
- `source_kind` 决定 Markdown 或 HTML 的规范化序列化。源码只有成功解析、清洗并生成
  AST 后才能保存；块编辑可以规范化标签、空白和 Markdown 写法。
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
- Agent 只允许 `search_books`、`read_passages`、`get_outline` 三个只读工具。宿主先
  计算授权 book IDs，模型参数只能缩小范围；保留工具轮次、结果数、上下文和超时限制。
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
   Reader/PDF Reader 还必须等最终稳定 locator 的阅读进度写入成功；写入失败时保持窗口
   打开并允许再次关闭重试，不能把“后台 worker 已退出”当作持久化成功。
5. 图书库窗口关闭只关闭该窗口，不调用 `App::quit()`/`cx.quit()`；否则会绕过其它
   Reader/Editor 的 close veto。若已有导入、创建、分组、移动或删除任务，先等待这些
   已接受 mutation 完成再移除图书库窗口；最后一个窗口关闭后让 GPUI 自然退出。
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

## 不可破坏的内容安全约束

- EPUB Reader 只提供 manifest 声明资源；保留编码路径穿越、非法路径/MIME 拒绝、
  内部 origin 白名单、CSP、`nosniff`、禁脚本/联网/外部导航/新窗口/下载和 incognito。
- PDF.js 必须完全本地加载，禁止 PDF JavaScript/XFA 和网络请求；协议路由只接受固定
  资产与当前 PDF，不得映射任意主机文件。
- Editor 的宿主初始化脚本是可信桥接，不代表允许书内脚本。IPC 必须校验 origin、
  href、session/revision/request-id、Ready/请求状态和大小上限。保存、切章、导出、
  关闭和 AI 引用必须取得完全匹配的快照，旧页面回调不能覆盖新章节。
- 打开 Editor 时只从当前 revision 的 `BookDocument` AST 生成会话投影，不读取或重新
  打包整本原格式容器，也不回退提供原件中的 CSS/字体/任意路径资源。持久媒体只在
  协议请求时经 `AppServices` 后台读取；关闭时先封闭协议响应屏障，再释放 WebView，
  禁止迟到任务向已销毁的 Wry responder 回调。
- 富文本 shell 与不可信正文保持 origin/能力隔离。Markdown 中仅允许白名单音视频
  HTML；RawHtml 必须清洗，外链、事件处理器和主动嵌入不得进入预览。
- 不要随意提高归档、正文、IPC、视觉页面或模型上下文大小上限。调整时补上限内、
  越界、取消与资源耗尽测试并说明风险。

## GUI 冒烟与完成标准

Debug 构建支持 `MOYE_DATA_DIR`；Release 构建忽略它并访问真实 LocalAppData。人工
GUI 验证必须使用唯一隔离目录：

```powershell
$env:MOYE_DATA_DIR = Join-Path ([System.IO.Path]::GetTempPath()) ("moye-agent-" + [guid]::NewGuid())
$env:RUST_LOG = "error"
cargo run --locked --bin moye-epub-editor
```

涉及 UI、WebView、导航、编辑器、Office 或窗口生命周期时，除自动验证外还要实际
走完受影响路径。按范围覆盖：启动；导入 EPUB/PDF/Office/DRM-free Kindle；结构化和
PDF 预览；新建图书；Markdown/HTML、目录、封面和媒体编辑；保存；三种搜索模式；
AI 侧栏与引用；原件/EPUB/PDF 导出并重新打开；多 Reader/Editor 窗口；WebView 构建中
与构建后关闭。

Office 相关改动要在安装和未安装 Office 的环境分别验证，并覆盖超时/取消/回退。
AI 相关改动优先以 mock OpenAI-compatible 服务验证 SSE、工具、范围和取消；真实
Ollama 冒烟不得自动启动服务或下载模型。媒体改动要覆盖 seek/Range 和缺失资产。

关闭图书库但保留子窗口、再关闭最后一个窗口时，不应丢失保存内容，且
`RUST_LOG=error` 不应出现 HWND/WebView 错误。在非 100% DPI 下自动化时区分截图逻辑
坐标与输入物理坐标，优先使用控件命中或 DPI 感知坐标。

没有实际执行 GUI 路径时，应明确写成“未做人工 GUI 验证”，不能仅凭编译或单元测试
宣称运行时问题已经解决。交付前报告实际命令、结果和未验证边界；Rust 改动至少通过
格式检查、全目标编译和全目标测试，运行时界面改动还需完成与风险相称的 GUI 验证。
