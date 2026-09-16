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

- `src/main.rs`：WebView2 探测、启动顺序与 GPUI 主窗口。数据目录不再用默认位置后，
  启动分成两段：`startup::plan_launch` 只判断能不能直接用记住的目录，需要用户确认时开
  设置窗口（`src/ui/data_dir_setup.rs`）；窗口确认后才完成待办搬迁、装日志、打开
  `AppServices`、开主窗口。顺序不能反：搬迁必须排在日志与图书库之前，否则旧目录已经被
  日志文件或数据库打开，整体重命名会失败。这些都在 `cx.background_executor()` 上跑：
  数据目录可能在网络盘上，GPUI 回调里阻塞会直接卡住界面。打开图书库/主窗口失败时清除
  记录并把原因显示回设置窗口（没有设置窗口就先开一个），不把坏目录留在配置里让每次启动
  都停在同一条错误上；搬迁失败则**保留**配置（`StartupFailure::forget_config` 为 false），
  下次启动继续重试，原因同时写进 stderr（那时文件日志还没装）。日志写不进去只降级为
  控制台并把原因写进 stderr —— 此时文件日志还没装，弹原生对话框会撞上 GPUI 的 `App`
  借用。
- `src/startup.rs`：数据目录的解析与引导配置。数据目录不再固定用 `ProjectDirs` 的
  默认位置：首次启动（或上次选择已失效）时由用户在设置窗口确认，选择写进**图书库之外**的
  `bootstrap.json`（`%APPDATA%\ngy\ngy_book_studio\config\`），下次启动直接使用。
  `settings` 表在图书库里，所以它不能用来记录图书库的位置。本模块不碰界面：
  `plan_launch` 返回 `Ready` 或 `NeedsSetup { reason, suggestion }`，`suggestion` 就是
  设置窗口输入框的初值（首次运行是推荐路径，上次目录失效时是上次的选择）；用户点确认后
  才由 `apply_data_dir` 校验并落盘。目录在写入配置之前必须通过 `ensure_data_dir_usable`
  （建目录 + 写删探针），避免把只读目录记下来让每次启动都停在同一条错误上；配置读不出来
  按“重新询问”处理并说明原因，不静默回落默认目录。`normalize_input_path` 把输入框文本
  整理成路径（去首尾空白与成对引号）。
  `NGY_DATA_DIR` 只在 debug 构建生效、优先级最高且不写配置，用于测试与隔离环境。
  `shell_dialog_directory` 是 native-dialog 唯一需要的 `\\?\` 前缀转换点（设置窗口的
  「浏览…」与学习中心恢复对话框都走它）。**对话框初始位置必须是存在的目录**：Windows 上
  native-dialog 传给 `wfd`，`SHCreateItemFromParsingName` 解析不存在的路径会失败并让整个
  对话框打不开（首次启动时推荐路径恰恰还没建出来），所以位置一律经 `dialog_start_directory`
  取存在的目录 —— 输入路径不存在就沿父目录上溯，一个都取不到就不设置位置。
  更改数据目录走 `apply_data_dir_change`：只写配置并记下待搬迁的旧目录（`move_from` /
  `move_overwrite`），搬迁由重启后的新进程在打开图书库**之前**用 `complete_pending_move`
  完成 —— 当前进程占着旧目录里的库和日志，运行中搬不动。搬迁语义是「新目录成为旧目录的
  完整副本、旧目录清空」：同卷先删掉空的目标目录再 `rename`，跨卷复制成功后才删源目录
  （删源失败只记警告，数据两份都在）。目标非空即报错 —— 合并两个书库没有明确定义；目标
  已有 `library.db` 时只在用户确认过覆盖（`move_overwrite`）后先删掉它自己的库文件 ——
  清掉之后仍非空一样报错。搬迁失败**不清配置**：
  `move_from` 就是下次重试的线索，用户换成别的空目录也照样保留。`complete_pending_move`
  只在本次启动真正要用配置里那个目录时才搬，`NGY_DATA_DIR` 覆盖的隔离环境不会把真实
  书库搬走。
- `src/ui/data_dir_setup.rs`：数据目录设置窗口。输入框预填推荐路径或上次的选择，可直接
  编辑、可用「浏览…」调系统目录选择器；点「确认并启动」或回车后才校验目录并写配置，
  校验（`startup::apply_data_dir`）在后台执行器上做。目录不可用、图书库打不开都把原因
  显示在同一个窗口里让用户改路径重试（`report_failure`），启动期间不接受第二次提交也不
  响应关窗；确认成功才交回 `src/main.rs` 打开主窗口，然后关掉自己。它不写真实
  `bootstrap.json` 的路径由调用方注入，因此可测。
- `src/logging.rs`：日志目录、默认过滤串与保留策略。全局 subscriber 由
  `main.rs::install_logging` 调用 `ngy_utils_tracing::init` 安装，安装点必须晚于数据目录
  确定（`LOG_DIR` 由它推出，此前的启动阶段没有日志）。文件层只在 Production/Test 模式
  落到 `<数据目录>/logs/ngy-book-studio.<YYYY-MM-DD>.log`，按 UTC 日期滚动，启动时清理
  超过 7 天的同类文件（只认自己的前缀 + 合法日期，其它文件不动）；Development（默认）
  只写控制台，模式可用第一个命令行参数覆盖。日志目录不是独立选项：它固定是
  `<数据目录>/logs/`。`main.rs` 持有 init 返回的 guard 并在进程退出前 drop，以免丢掉
  非阻塞写入的尾日志；init 失败退回 `console_tracing`，两条路都装不上只报 stderr，
  不 panic（GPUI 回调不可 unwind），也不拦住启动。
- `src/services.rs`、`src/runtime.rs`：进程级服务组合与独立 Tokio runtime；统一持有
  图书库、对象存储、格式注册表、搜索、AI、Office 和后台任务。GPUI 回调跑在自己的
  executor 上，不是 Tokio 上下文：UI 可达的服务方法必须把数据库、对象存储、`tokio::fs`
  和解析工作放到应用 I/O runtime（`IoRuntime::spawn`）上再 await，`runtime.rs` 的
  `block_on` 只用于启动、测试与后台线程。索引协调器内部 `run_db`/`run_db_mut` 解析
  环境句柄，只允许它自己派发的工作使用；公共路径用 `run_db_on` 显式传入句柄，否则会
  在 GPUI future 里 panic，而 GPUI 回调不可 unwind，会直接终止整个进程。
  `src/services.rs` 测试里的 `block_on_without_tokio` 用非 Tokio 执行器驱动服务方法，
  用于固定这条约定（后台任务窗口的文本块明细读取曾因此崩溃）。
  `load_published_visual_pages(book_id, kind)` 是 Office 增强预览与 DjVu 共用的页面读取
  入口：它在 `library_mutations` 与 `blob_publication` 门闩内读 `visual_pages`、逐页校验
  长度/BLAKE3/`BlobKey`/图片格式后才返回字节，`load_office_enhanced_pages` 只是它的
  委托包装；新增页面来源时必须走同一入口，不要复制门闩或摘要校验。
  `ensure_visual_render_job` 只用于本地可安全自动运行的渲染（DjVu），Office COM 增强
  必须继续走显式信任对话框与 `set_office_enhancement_enabled`。
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
- `src/formats/`：`DocumentImporter` 注册表及 EPUB、PDF、Office、Kindle、KFX、DjVu
  适配器；`office_oxide`、`ebook-rs`、`djvu-rs` 等第三方类型必须在本目录内转换为统一模型。
  导入器必须把来源声明的语言写进 `BookDocument.language`：Kindle/KFX 取 `ebook-rs`
  元数据，EPUB 取 OPF 的 `dc:language`（`rbook::EpubMetadata::language()`）。
  `books.language` 是翻译任务跳过「本书已是目标语言」的唯一依据
  （`transactions::reconfigure_translation_jobs`），EPUB 曾经漏填，于是连声明了
  `zh-CN` 的中文书都会被排进翻译队列；PDF/Office/DjVu 没有可声明的语言标签，导入后
  仍是 `None`，这些格式不会因此跳过翻译。
  `kindle.rs`：`ebook-rs` 只认 PalmDOC（压缩 1/2），因此 HUFF/CDIC（压缩 17480，
  `kindlegen -c2` 与多数 Amazon KF8 文件使用）由 `kindle_huff.rs` 自行解码后重写为
  未压缩容器再交给解析器。重写必须保持记录索引不变——正文按头部已声明的
  `text_record_count` 个槽位分片，其后所有记录原样复制，`first_image_index` 与
  `recindex=` 重写才继续成立；解码前必须先按 `extra_record_flags` 剥掉记录尾部的
  数据区，否则填充位会被解码成杂散符号；拼接后按 PalmDOC 的 `text_length` 截断，
  末条记录的位填充差异只体现在该处。HUFF/CDIC 只改正文压缩方式，仍保留字节一致的
  原件，加密（`encryption != 0`）与 DRM 一样继续拒绝。
  **`extra_record_flags` 非零时正文记录尾部带数据区，PalmDOC 路径同样必须先剥。**
  `ebook-rs` 把 `text_record_count` 条记录原样拼接后才试 UTF-8，失败就整段回退
  WINDOWS-1252；尾部数据区正好让这一段不是合法 UTF-8，于是中文按 CP1252 逐字节
  解码，阅读页显示 `ä½œè€…ç®€ä»‹` 而不是 `作者简介`（英文书 ASCII 两种编码相同，
  所以只有 CJK 书暴露）。区域在压缩流之外：位 15..1 是区域，大小写在区域末尾的
  大端 varint，位 0 的多字节重叠字节最后剥；剥掉后记录保持原压缩方式，解码仍交给
  `ebook-rs`。两条路径的容器重组都走 `repack_palm_db()`，不要各写一份偏移重算。
  `ebook-rs` 对 PalmDB 既不给资源清单也不给封面（`MobiBook::parse` 的 `opf.manifest`
  恒空、`cover_href` 恒 None），`kindle:embed:` 因此没有可解析的资源，共享的重写遍历
  会把整条 `src` 删掉——资源必须由本模块自己扫记录表：MOBI 头 `0x6C` 是第一个资源
  记录，从它起的每条记录（无论是不是图片）都占一个号，`kindle:embed:<base32>` 与
  MOBI `recindex` 指的都是这个从 1 起算的号，封面号是 EXTH 201 相对同一起点的偏移。
  `kindle:embed:` 的编号是 base32，而 `ebook-rs` 只替换它按十进制补零恰好命中的那几条
  （`0001`..`0009`），其余原样留下，两种写法都要映射回同一个号。它同样不读 MOBI 目录：
  切分只认 `<h1>`/`<h2>`/`<h3>` 并给每段贴上 `Section <n>`，转换过的书会被切成几百段，
  因此章节改为按“带文字的一级标题”分组，标题取该 `<h1>` 的文本；KF8 的样式表存放在
  flow 记录里，会以不带标签的裸 CSS 落在正文末尾，必须在进模型前清掉。章节标题取可见
  文本时要解命名实体，而那个扫描窗口必须按字符边界收口（`entity_scan_window`）：
  `str::len()` 是字节数，中文标题里一个没写成实体的 `&`（`Tom & Jerry 汤姆和杰瑞历险记续篇`）
  会让第 34 字节落进三字节字符内部，`&value[..len.min(34)]` 直接 panic 掉整次导入。
  `kfx.rs`：`ebook-rs::KfxBook` 是启发式文字抽取而非完整 KFX/Ion 解析（`resources` 恒空、
  metadata 有占位默认值），因此只保留原件 + 抽取正文，导入前必须过 `validate_kfx_text`
  三项守卫（可见字符下限、无法解码字符比例、成词比例），解析不出正文一律拒绝，
  不得放宽为“导入成功”。`djvu.rs`：字节所有权要求下先校验上限再克隆一次源字节；
  隐藏文字层经 `reflowable_text` 转页文字，NAVM 书签只有能解析出 `#<页号>` 时映射
  （其余跳过但保留可解析子项）；导入阶段不产出页面图片。
- `src/markup.rs`、`src/editing.rs`、`src/export.rs`：HTML 解析清洗、事务式
  模型编辑，以及原件/EPUB/PDF 稳定导出。
  **XHTML 只能出现 XML 能自行解析的字符引用。** 阅读器把章节按
  `application/xhtml+xml` 交给 WebView，除 `&amp;` `&lt;` `&gt;` `&quot;` `&apos;`
  和数值引用外，任何命名实体都会让 WebView2 用 "Entity 'nbsp' not defined" 的
  解析错误页替换整章。所有 HTML 序列化器都会踩这一点：ammonia/html5ever 把 U+00A0
  写成 `&nbsp;`（`sanitize_html`、导出的 `sanitize_raw_html`），面向 HTML 解析器
  编写的 EPUB 又常用 `&mdash;`、`&ldquo;`。因此凡是要嵌入 XHTML 的片段都必须过
  `markup::xml_safe_entities()`：它按 html5ever 的实体表把这类引用改写成数值引用
  （U+00A0 的引用在多码点实体处也逐码点展开），文字内容不变，找不到的引用名原样保留。
  `markup::serialize_xhtml()` 走 `write_xhtml_value` 已满足该约定（编辑器用它）；
  `export.rs::render_epub_unit()` 在拼好整章正文后统一改写一次，因为 `render_blocks`/
  `render_inlines` 的 `RawHtml` 分支直接落 ammonia 输出。`reader::load_resource_with_range()`
  在服务边界再做一次，覆盖未经过我们导出器的第三方 EPUB 章节。新增 XHTML 组装点必须
  走同一层，不要各自再写一份转义。
- `src/storage.rs`、`src/media.rs`：应用自有 `BlobStore`、基于 `object_store` 的本地
  BLAKE3 内容寻址实现，以及带图书归属校验和 Range 支持的媒体响应。
- `src/library.rs`：SQLite 与对象存储之上的图书库业务编排、导入/创建/保存/删除、
  垃圾回收和兼容现有 UI 的投影。
  打开图书库时按顺序做两件一次性修复：先 `run_startup_blob_gc` 回收未引用对象，
  再 `run_startup_language_backfill` 把导入器漏记的声明语言补回 `books.language`。
  回填只读容器元数据（`formats::declared_language`，EPUB 取 OPF 的 `dc:language`），
  不碰正文、不覆盖已有语言、不动 `revision` 与 `updated_at`，因此索引、译文、笔记与
  排序都不受影响；完成标记 `library.language_backfill.v1` 写进 `settings`，让重读原件
  只发生一次（原件可能很大，不能每次启动都读）。空库不写标记——没有书可查就不该
  提前用掉这次机会；有条目读不出来或解析失败也不写标记，留给下次启动重试。
- `src/annotations.rs`、`src/db/annotations.rs`：三种划线、人工想法与 AI 想法统一使用
  `annotations` 一张表。宿主按实际阅读章节 body 文本（排除 script/style/noscript/template，
  移除 ECMAScript 空白）的 UTF-16 起止位置校验 quote、书/单元归属及打开时的版本。
  修订或删除章节保留失效笔记，不按 quote 搜索重定位；删除图书级联清除笔记。
  同书/单元/双版本/精确起止范围的三种标记由部分唯一索引约束，改样式在事务中替换，
  不创建多条标记。删除划线只删除该范围的标记，人工与 AI 想法保持不变。
  当前开发结构版本为 14，遵循重建策略，不编写迁移。
- `src/db/`：SQLite 连接、当前结构、单表 CRUD/查询映射和跨表事务。每张表对应一个
  文件：`books.rs`、`book_sources.rs`、`content_units.rs`、`toc_entries.rs`、
  `blobs.rs`、`assets.rs`、`asset_refs.rs`、`progress.rs`、`search_chunks.rs`、
  `embeddings.rs`、`index_jobs.rs`、`visual_pages.rs`、`visual_page_staging.rs`、
  `chat_threads.rs`、`chat_messages.rs`、`chat_citations.rs`、`groups.rs`、`settings.rs`、
  `translations.rs` 和
  `office_enhancements.rs`；跨表原子操作只放 `transactions.rs`，FTS 查询放
  `book_search.rs`，建表与完整性契约放 `schema.rs`。
- `src/search.rs`、`src/indexing.rs`：作用域内 FTS5/`sqlite-vec` 精确 KNN、RRF 混合
  召回，以及可恢复的 embedding/vision 后台任务。执行器固定启动
  `MAX_BACKGROUND_JOB_CONCURRENCY` 个 worker，只有序号小于当前并发的 worker 扫描队列，
  每个任务提交后按配置间隔休眠；两者由 AI 设置“后台任务”经
  `IndexingCoordinator::configure_scheduling` 实时发布，写坏的值按范围钳制。
  同一个「任务并发」既是任务上限，也是**单个整本翻译的块窗口上限**：一轮里最多同时开
  `concurrency` 个文本块请求（`walk_translation_blocks` 的窗口，每轮重读设置，所以运行中
  调大只让 walk 走得更前、不打断任何在飞请求），因此一本翻译自己也会出现多个「处理中」。
  `indexing.rs` 另实现整本图书翻译任务
  `kind="translation"`：任务标识为 `translation:<source_id>:<target_language>`，游标用
  `next_ordinal` 记录「第一块尚未提交的文本块」，并按序调用对话模型 `chat_stream` 写入
  `translations` 表，可暂停/恢复/重试/取消；文本块按 `content_units.block_json` 的 `BlockDocument` 确定性
  提取（段落、标题、引用、列表项、表格单元格；代码保持原样），以
  `(document_revision, unit_revision, target_language, 对话模型)` 判定失效并重译。目标语言
  或对话模型变化由 `AppServices::configure_translations` 经
  `transactions::reconfigure_translation_jobs` 重排；每本当前来源只保留一个目标语言任务。
  源语言（`books.language` 主语言子标签）等于目标语言时跳过；翻译任务未配置对话模型时
  失败而不猜测。模型调用不得逐 token 打日志，也不得记录正文或译文内容。
 凡是只认 `content` 里严格 JSON 的调用（翻译、vision、Agent）都固定发送
 `reasoning_effort="none"`：思考模型会把整段输出预算和请求超时全花在推理上，Ollama 的
 OpenAI 兼容端点对思考模型只回空 `content` 增量，表现为整段超时里 0 字节正文（本机 9B
 模型实测 2886 个空块）。三条路径不得各自漂移。
  **EPUB 章节在数据库里保存成单个 `RawHtml` 块**（`<div id="sbo-rt-content">…` 之类），
  语义块结构只存在于 HTML 里：只走 `BlockDocument` 会得到 0 个文本块并让任务立刻“成功”。
  因此必须处理 `Block::RawHtml`，用 `markup::translation_blocks_from_html()` 取出最内层
  块级元素（`p`/`h1–h6`/`li`/`blockquote`/`td`/`th`）的规范化全文和非空白文字叶节点。
  原生 EPUB 必须从当前 source blob 用 `OpenedBook`/`load_resource` 提取实际阅读章节的
  body；校验 spine 数量、单元顺序与归属。不能用导入后的 AST 代替原生章节，它可能合并
  链接、span 或列表文字叶节点。其它格式使用规范化 HTML；解析在后台执行。
  `src/translation.rs` 定义只含文字的分段协议：每次提供整段上下文与整数 ID，模型必须
  返回完整、唯一的 ID 集合；按原文顺序回排并恢复片段边界空白，不能用模型 HTML 替换正文。
  响应允许完整的前置 `<think>`、Markdown 围栏和说明文字包裹一个完整答案；规范外壳是
  `{"translations":[{"id":0,"text":"译文"}]}`。只有小模型丢掉该外壳时才把顶层片段对象本身或
  片段对象数组折进同一外壳（现场：`qwen3.5:0.8b` 逐块返回 `[{"id":0,"text":"…"}]`，整本书
  的块都判 `invalid_schema` 而任务失败），元素仍必须是严格片段对象——位置化片段、带额外字段
  的元素和 `[{"translations":[…]}]` 这类包裹别的答案的容器一律拒绝，绝不搜索嵌套结果。多个
  答案、截断 JSON、缺失/重复/未知 ID 均拒绝。
  解析失败时只允许一次**结构标点归一**：把字符串字面量之外的全角结构字符（`：，｛｝［］＂“”`）
  换成半角 ASCII 后再走同一条严格流水线。它只改写 JSON 里只可能是结构的位置，因此合法
  响应永不进入该路径、译文内容逐字不变；键名吞掉分隔符（现场 266 块 `"text："`）要恢复
  只能猜模型意图，必须继续拒绝并保留原始终止行列。格式说明必须显式要求结构字符用半角
  ASCII、全角标点只能出现在 `text` 值里，纠正提示按固定失败分类复述要修的部分。
  分段协议校验失败时使用冻结的原始输入和服务额外纠正一次，不回传模型的错误输出，
  不把纯文本猜分段，也不为 HTTP、流中断这类 provider 失败做协议纠正。重试前后检查任务
  控制与身份，失败沿用本执行游标，不能读取新任务游标后将新任务标为失败。
  **块级重发（2026-09-15 起，`TRANSLATION_BLOCK_RETRIES = 5`）**：一个文本块的一轮失败后，
  整块按同一份冻结输入重发，最多 5 次，所以一个块最多 6 轮（一轮 = 两次严格尝试 + 一次协议
  纠正）。重发轮的第一发带着上一轮的失败分类，因此重发不是逐字相同的请求；每轮开始时先在
  同一把 `transitions` 门闩内复查控制请求与来源版本。**只有协议类失败（`ResponseError`）才
  重发**：provider、数据库与取消错误立即上抛让整次运行失败，重发同一个请求解决不了它们。
  失败分类跨轮累积（`rejected_kinds` 属于整块而不是某一轮），否则轮 1 的
  `segment_count_mismatch` 会被后面轮次的 `incomplete_json` 顶掉，最后一轮就不肯走逐片段
  回退。逐片段回退只留给最后一轮：它是整块的最后手段，前面几轮只做严格尝试与纠正，免得一个
  多片段块把请求预算耗在重复的逐片段请求上。注意 `TRANSLATION_TEMPERATURE` 恒为 0，同一份
  请求重发在确定性模型上会拿到逐字相同的答案（现场：块 3579/3620 在两次运行里的响应字节数
  完全相同），重发真正救回的是截断与采样抖动这类非确定性失败；要救「确定性不合格」的块，
  只能靠失败分类不同的重发提示，而不是重发次数。
  纠正在两次请求后仍失败的文本块保留原文并继续翻译其余块：按块记 `ProtocolSkipped` 警告（块序号、总数、固定分类），
  并像缓存命中一样持久推进游标（执行器要求内存游标与持久游标始终一致）。**没有连续跳过上限，
  也没有「整本图书没有任何有效译文」的中止条件**：跳过多少块都要一直走到书末，运行一律按
  `RunOutcome::Succeeded` 收尾，游标永不回滚（2026-09-15 起按用户要求「失败了就跳过，直到整本
  书的文本块都运行完」；原 `MAX_CONSECUTIVE_TRANSLATION_SKIPS` 常量与
  `publish_untranslated_failure` 回滚入口已删除）。跳过因此不再有运行级表现：只有 provider、
  数据库与取消失败才让整次运行失败，「这批块没译出来」只存在于文本块明细里。
  **块级并行（2026-09-15 起）**：一次整本翻译按「任务并发」同时打开多个文本块请求，窗口用
  `JobCursor.inflight`（升序块号）记录，只有整本翻译会填它，其它任务恒空。三条规则必须同时
  成立：① 提交严格按 ordinal 顺序（`resolved: BTreeMap` 只在 `next_ordinal` 处出队，
  `TranslationRun::commit` 先 `next_ordinal = ordinal + 1` 再 `close_inflight`），因此
  `next_ordinal` 永不越过还在等的块，被中断的运行只会重做、绝不会把没翻完的块当成已完成；
  ② 控制检查与游标写入必须在同一个 `transitions` 门闩内（否则一次按块重试重置出来的新游标
  会被这一轮的旧游标覆盖），但等 provider 响应时绝不持锁；③ 暂停/取消/换版本/被替换一律先
  `settle()` 清空窗口并落盘，再按**清空后的**游标发布结果 —— 发布里的 JSON 必须与库里那份
  逐字相同，否则 `finalize_running_from_cursor` 匹配不上，任务卡在「处理中」；`Abandoned`
  一个字节都不写。块请求不借用游标：它拿到的是 `TranslationBlockScope`（块号、revision、
  运行最后持久化的 JSON），每次控制轮询拿它与库里那一行比对，所以窗口前进不会被误判成
  「这一行已经不属于我」。
  被跳过的块靠明细里的两条重试路径修复，都必须复用同一个 worker 与同一套跳过规则：页顶
  「重新翻译」前的「重试失败块（N）」重跑本次全部未翻译块，块行内「重试」只重跑该块
  （`retry_translation_blocks(job, vec![ordinal])`，**不得顺带重发其它失败块**）。实现方式是把
  目标序号写进游标 `retry_ordinals` 并把 `next_ordinal` 退回最小的目标序号，让 worker 只走这一段：
  非目标块照常推进游标但不发请求。请求里已经有译文的块会被过滤掉，所以重试不会覆盖已有译文或
  人工修订；从 `paused` 起始的翻译任务也要能重试（新任务默认暂停），这条走独立的
  `reset_for_translation_block_retry`，不得放宽通用的 `reset_terminal`（视觉替换依赖它拒绝
  活跃任务）。
  「截断不额外纠正」不等于「让整次运行失败」：`finish_reason=length` 的截断必须按固定
  分类 `response_truncated` 返回，才能落进上面这条按块跳过、推进游标的规则。它没有运行级
  含义，同一种请求换个请求形状只会再截断一次，因此既不额外纠正也不做逐片段回退；返回成不
  透明的错误则会让整本书永远停在同一个退化文本块上——现场 2026-09-15，block 1762
  （`qwen3.5:0.8b`，3 个片段）：两次运行都在 4096 个输出 token 处截断，响应各 4425 字节、
  一个完整容器都没有，任务只有 attempts 在涨、`next_ordinal` 不动。块级重发对截断同样生效
  （每轮一发、每次都在同一个上限处停下），用尽重试后才按块跳过。
  整块重发的**最后一轮**里、两次严格尝试都用完时，**多片段文本块**再走一次逐片段回退
  （`SegmentFallback`）：每个文字叶
  单独一次请求，`source` 与 `segments` 都只含该片段本身，因此模型没有可越界翻译的内容，校验
  仍是同一套严格解码（单片段只要一个 `id=0` 的答案）。现场（2026-09-13，block 79/80/81，
  `qwen3.5:0.8b`）是弱模型把整段译文合并进 `id=0`，两次尝试后仍判 `segment_count_mismatch`，
  连续三块（当时连续跳过上限是 3，会结束整次运行；该上限已于 2026-09-15 取消，现在只会继续
  往下走）；主路径仍必须先是「冻结输入 + 一次纠正」那两次请求，回退只允许发生
  在这一步之后。回退只针对「模型答了、但片段不完整」的固定分类（`segment_count_mismatch`、
  `missing_segment_id`、`unknown_segment_id`、`duplicate_segment_id`、`empty_segment_text`、
  `invalid_segment_text`、`positional_segments`）：JSON 层面的失败说明模型没写出答案，换请求
  形状无用；片段数超过 `MAX_TRANSLATION_FRAGMENT_FALLBACKS` 的块也不回退，直接沿用跳过规则。
  **判定读整块的失败分类（跨重发轮累积），不是只读最后一次。** 纠正提示与块级重发都会改变
  模型写出的错误形状：现场
  2026-09-15（block 1786/1787，`qwen3.5:0.8b`）attempt 1 把 3 个片段合并进 `id=0`
  （`segment_count_mismatch`），纠正之后模型开始按段写、但 JSON 没闭合（`containers=0
  open_container=true segment_markers=3`，恰好等于期望段数），最后一次的分类因此变成
  `invalid_json` / `incomplete_json`。两次说的是同一个能力上限（写不完多段 JSON），而逐片段
  请求正是它能满足的形状；只看最后一次会把这两块白白跳过，连着 1785 一起被跳过（当时还会
  凑够连续三块、结束整次运行：`translation_run_finish result="failed" next_ordinal=1785`）。
  `positional_segments`
  是 `require_segment_objects` 单独报出的类别：模型按段答了、段数往往也对，只是把
  `{"id","text"}` 写成了 serde 可接受的位置序列（`{"translations":[[0,"译文"]]}`，现场
  block 1785，4 个片段写出 5/4 个标记）；它与真正结构无效的 `invalid_schema` 分开，
  任务日志仍沿用 `InvalidSchema` 分类。结果按原顺序拼成一条
  `StoredTranslation`（边界空白由 `parse_response` 从片段自身恢复，缓存键仍按整块原文计算）；
  任意一个片段仍不通过就整块跳过，绝不保存部分译文，provider/数据库/取消失败仍让整次运行失败。
  诊断使用 `ngy_ai`、
  `translation_run_id`、块序号、尝试次数、固定错误分类、JSON 行列/数量、响应字节数与
  流块分类计数（`content_events`、`empty_content_events`、`unrecognized_events`），
  不记录原文、译文、任意字段名或解析器错误正文。空 `content` 块与“本客户端不消费的块”
  必须分开计数：否则“120 秒超时、2886 个事件、0 字节正文”会被读成健康但缓慢的回答。
  逐块形状读数同样只含数字与固定标签：`translation_source_resolved`（提取走 `epub` 还是
  `canonical_html`、修订、对象字节数、是否校验原件章节路径）、`translation_blocks_extraction_start`
  与 `translation_blocks_extracted`（单元数、spine、多片段块与单元数）、`translation_block_start`
  的 `segment_chars`、每次尝试的 `system_bytes`/`user_bytes`/`expected_ids`、解码后的
  `translation_response_decoded`（`decoded_segments`/`decoded_ids`/`merged_into_one`）、逐片段
  回退的 `translation_fragment_*` 与收尾的 `translation_run_stats`（saved/cached/untranslated/
  回退次数）——它们是把「模型少写片段」和「模型没写出答案」分开的唯一依据，不得在其中打印原文。
  翻译流不再按总时长掐断：每 10 秒输出一条 `translation_stream_progress`（已用时、距上次
  事件的静默、正文块数/字节数、完整 JSON 容器数、未闭合容器、片段标记数、思考块状态、
  答案重复标记），流结束时把同一组形状计数写进 `translation_stream`，失败时另存
  `translation_response_salvaged`。这些计数只说明“模型没写出答案 / 思考没结束 / 答案重复
  输出 / 容器被打断”，不含任何正文；`NGY_DUMP_TRANSLATION_RAW` 仍是唯一会打印响应与
  冻结输入的开关，并且现在也覆盖流失败。流没有正常结束（含 `ProviderTimeout` 静默超时）
  但已收到的字节能通过完整分段校验（只有一个完整容器、id 不多不少）时采用该答案并记警告；
  两个答案、缺片段、容器被截断或校验失败一律不猜、按原样失败，此时 provider 失败仍让
  整个任务失败。
  `pre/code` 内容计入全文匹配但不翻译，过滤 script/style/noscript/template；换行节点由
  源 DOM 保留，空白规范化与 ECMAScript `\s` 一致。**代码块整体不翻译**：除 `pre`/`code`
  子树外，未标记但整段是代码的块（Calibre/Word 转换把每行代码放成独立 `<p>`，验收 EPUB
  的 ZWSP 缩进代码行）也要跳过——`markup::looks_like_source_code` 与 `translations.js` 的
  `looksLikeSourceCode` 必须使用同一套规则与同一份表：任一行去掉 ECMAScript 空白、ZWSP、
  ZWNBSP、软连字符后以 `;`/`{`/`}` 结尾、以 `//`、`/*`、`*/`、`#!`、`#include`、`#define`、
  `#pragma`、`<!--` 开头、整行是 `<…>` 标记，或含 `=>`/`->`/`::`/`:=`/`==`/`!=`/`<=`/`>=`/
  `&&`/`||`/`+=`/`-=`/`*=`/`/=`/`</`/`/>` 之一即判为代码。裸 `=` 与全角 `；`/`：` 不算信号，
  行内 `<code>`（正文提到代码）不影响该段翻译；两侧规则不一致会让代码块吃掉后续同文本
  正文块的译文。判断只看非代码叶子的行（`<br>` 记为换行，匹配文本不变）。
  三组信号是 2026-09-15 补上的（现场《RUST AND SCALA FOR BEGINNERS》azw3，块 3579/3580/
  3588/3620/3623）：行内含字符串拼接（`+"`、`"+`，含空格形式）或调用/下标里的引号
  （`("`、`")`、`('`、`')`）、含转义序列（`\n`、`\t`、`\"`、`\\` …）、以语句关键字
  （`def`/`fn`/`let`/`const`/`struct`/`return`/`println`/`printf`/`console`/`system` 等 25 个）
  开头且同一行还出现 `(`/`=`/`{`/`[`、出现无空格的类型注解（`a:Int`、`):Int`）。这些块此前
  逃过判定后，模型要在 JSON 里转义 `"\n"` 这类字面量，两次尝试加逐片段回退仍判
  `segment_count_mismatch`/`incomplete_json`/`empty_segment_text` 而被跳过。**孤立的引号
  永远不算信号**：散文把引用写成 `He said "hello" to me.`，只看引号会把每段带引文的正文都
  留在原文；关键字同样必须与同一行的代码形状一起出现（英文句子也会以 `let`/`use` 开头）。
  代价是含代码字段名的散文段会被一起跳过（实测这本书 2961 个散文块里 3 个），保留原文
  比送进模型更安全。**改动这套规则必须把 `indexing::TRANSLATION_BLOCKS_VERSION` 加一**：
  块数会变，旧游标的序号不再指向同一块，`JobCursor::from_job` 见到版本不符就把翻译任务
  退回书首重扫（已存译文仍按缓存键命中，重扫不花请求）。
  文本叶节点必须去掉 ECMAScript `\s`
  和 Unicode `Cf` 格式字符后仍有可见字符，才能成为翻译槽：EPUB 常用 ZWSP 缩进代码行，
  ZWSP 不属于 `\s`，送进协议后模型只会回空白，`empty_segment_text` 会拒绝该文本块
  （现场 96 号代码行）。`markup` 与 `translations.js` 必须共用同一份字符表，两边叶节点
  数量不一致会让回填整体对齐失败而保留原文。该规则只会减少槽位，命中旧缓存的块最多回退
  原文，因此不提升 `translation-v2` 身份、不重译整库。源文本超过既有上限明确失败，不能
  截断后存成完整块。`translated_text` 保存校验后的 `StoredTranslation` JSON 和执行身份。
  阅读窗口可以手工改写单个文本块的译文，它存在同一行的可空 `translations.manual_text`（同形状
  JSON），与机器文本**并排**：`upsert`、缓存命中重挂与重排事务只替换机器列，必须保留手工列
  （重排确实要作废时走 `delete_for_retranslation`，它同样保留仍可显示的手工行），因此后续模型
  运行、重译、重启或换端点都不会覆盖读者写下的文字，「恢复机器译文」只是把该列写回 `NULL`。写入只走 `AppServices::set_manual_translation`：来源永远取自表内片段
  （`manual_translation_segments`，请求只能替换译文文本），并按
  book/unit/block/language/document_revision/unit_revision 精确匹配，匹配不到或校验失败即报错
  且不落库。读取时手工译文**不校验模型与执行身份**（读者自己的文字不能因换模型而消失），但
  文档/单元版本仍必须匹配，手工列无法解析时退回机器译文。结构版本 13 → 14 只为这一列。
  对话模型或端点变化不清除旧译文：重排事务按协议前缀
  （`translation::EXECUTION_IDENTITY_PROTOCOL`，即身份中冒号前的 `translation-v2`）判定
  可复用后，把该书该语言的行重新盖上当前模型与身份并保留游标位置，只有未翻译的块才由
  新引擎补翻；协议版本变化（如 `translation-v2` → `translation-v3`）仍整本作废重译，
  无法解析的旧纯文本行同理。缓存读取与执行仍逐行核验身份，避免同名模型换端点后
  在未授权的情况下复用旧结果；用户要干净重做时用「重新翻译」。
  编辑保存按**章**增量失效：`publish_document_with_assets` 只给内容哈希变化的章升 revision
  （`PublishedDocument.unit_revisions` 把真实修订号交回调用方，编辑器不得自行假定所有章都升
  版），发布事务保留 `content_units` 行（只删除真的离开文档的章），并把未变章译文行的
  `document_revision` 刷到新版本，因此其它章的译文在编辑后仍然有效。笔记不受此影响：其失效
  判定仍要求 `document_revision` 等于当前书版本，未改动章也会显示为失效，不要声称笔记同步保留。
  翻译任务的文本块列表始终覆盖当前来源的全部章，是否复用某个块由「原文文本 + 叶子切分 + 章节
  版本 + 文档版本 + 模型身份」共同决定（`translation_cache_key`），不能按块位置判定：规范化
  EPUB 投影会给没有同名标题的章节补 `<h1>` 并整体移动块下标，按位置判定会让整本图书白翻一遍。
  命中时把已存译文重挂到当前块 ID（不调用模型），使行始终跟随它所属的块。
  「重新翻译」由 `IndexingCoordinator::retranslate` 实现：先把该书该语言的译文行删掉
  （`db::translations::delete_for_retranslation`），再把游标 `next_ordinal` 归零并重置为
  queued/paused；只对 `translation` 生效，来源已被替代或其它任务类型一律拒绝（不改状态）。
  这次删除**保留仍可显示的手工译文行**（`manual_text` 非空且 document/unit 版本都是当前的），
  换模型/协议重排走的是同一个函数：后台动作不得删除读者写下的文字。被保留的行仍是有效的
  缓存项，所以重新翻译只重跑其余文本块，手工改写过的块保留原机器译文与身份；版本已经过期
  的手工行和没有手工译文的行一样被删掉，不会留下再也显示不出来的孤儿行。
- `src/preview.rs`、`src/windows_pdf_renderer.rs`、`src/djvu_renderer.rs`：`VisualRenderer`、
  结构化页面 PNG 光栅化、Windows PDF 原页光栅化与纯 Rust DjVu 逐页光栅化、
  可暂停/恢复/重试/取消的持久任务和本地 PDF.js 资产路由。DjVu 渲染器（`ngy-djvu-png`）
  不加 `cfg(windows)`，页与内容单元严格 1:1（`content_unit_id` + `SourceLocator::DjvuPage`），
  必须在 `db::transactions::{renderer_for_source, visual_job_spec}` 里按 `format == "djvu"`
  选中；新增 renderer 时这三处（注册、恢复选择、任务构造）必须同时更新，否则导入时
  创建的 `visual-render:{source_id}` 任务会退回结构化 SVG 渲染。
  渲染器选择失败/未注册会在恢复期报「找不到当前来源所需的 renderer」，不要用
  `ngy-structural-png` 兜底掩盖 DjVu 页面缺失。
- `src/ai.rs`、`src/credentials.rs`：OpenAI-compatible models/chat streaming/embeddings
  接口、端点策略和 Windows Credential Manager 密钥存储。
  端点配置的“请求超时”不再作为 reqwest 的整段请求超时：`Client` 只保留 10 秒连接超时，
  非流式调用（models/embeddings）由 `within_request_timeout` 按总时长约束，流式回答由
  `ProviderTimeout::stream` 约束“两个数据块之间的静默”，因此持续输出数据的慢模型不会被
  中途掐断（2026-09-11：120 秒整段超时切掉了一条仍有 1579 个正文块、7024 字节正文、
  没有 `[DONE]` 的流，整本图书被判失败）。流的总规模仍由 `max_tokens`、32 MiB 字节上限和
  65536 事件上限约束。`ProviderTimeout` 在 `ai_diagnostics` 中映射为固定分类
  `http_timeout`，后台任务日志与 UI 不因超时改在块间生效而出现新类别。
  `SseDiagnostics` 每 10 秒输出一条与内容无关的传输进度行（`sse_progress`：字节数、事件数、
  正文块数、空块数、距上次事件的时间、最大间隔），结束时不再只给分类计数。
  **AI 与联网搜索的客户端默认跟随系统代理**（reqwest 自己读 `HTTP_PROXY`/`HTTPS_PROXY`），
  而默认端点正是本机 Ollama，代理会把它一并接管；「系统配置」的「使用系统代理」开关
  （`ai.network.preferences.v1`，默认开）关掉后走 `OpenAiHttpProvider::new_with_proxy` /
  `HttpWebSearch::new_with_proxy` 的 `.no_proxy()` 客户端。代理是系统级偏好，因此**不进
  `ProviderConfig`**（endpoint 级结构，且被大量测试字面量构造）；两处客户端都只在保存设置
  时重建，开关对已发出的请求无效。
- `src/agent.rs`、`src/agent_runtime.rs`、`src/agent_chat.rs`、`src/chat.rs`：只读 Agent
  工具、SSE/tool-call 循环、窗口授权范围、会话/消息/引用持久化与对话编排。
- `src/office_com.rs`、`src/office_preview.rs`、`src/office_visual.rs`：可选 Office STA
  工作者、只读临时导出和持久视觉渲染器；原件禁用宏和外链更新，派生页按当前
  renderer/revision/profile 发布，失败时回退结构化预览。
- `src/reader.rs`、`src/epub_limits.rs`：EPUB 投影、资源授权、导航 URL 和归档安全上限。
- `src/ui/`：按窗口/职责拆分的 GPUI 界面。`library.rs`、`reader.rs`、`pdf_reader.rs`、
  `editor.rs`、`page_image.rs` 分别管理对应窗口；`ai_sidebar.rs`、
  `ai_controller.rs`、`ai_settings.rs` 管理 AI 交互；`background_jobs.rs` 管理当前图书范围
  的派生任务；后台任务窗口全应用只有一个，重复打开时激活现有窗口并按新的库范围刷新，
  不打开第二个；`mod.rs` 只保留跨窗口主题、窗口打开与安全关闭基础设施，其中包含按
  图书登记的窗口表：删除图书后关闭该书已打开的阅读、PDF/Office 预览、DjVu 页面和编辑窗口。
  `page_image.rs` 是 Office 增强预览与 DjVu 共用的页面图片窗口，以
  `VisualPageSourceKind` 区分来源：它决定副标题文案、AI 引用资格（DjVu 每页 1:1 可引用，
  Office 重排预览页不可）与是否提供“本页文字”面板。缩放是呈现层的像素尺寸变化，
  不重新渲染；文字面板必须沿用 `scrollable_page_text` 的「外层滚动 + 自然高度
  `selectable(true).scrollable(false)` TextView」，不得恢复内部虚拟列表滚动。
  阅读进度复用 `reader::ReadingProgressWriter` 的有序写入与投影刷新，页变化时入队；
  打开 DjVu 会经 `AppServices::ensure_visual_render_job` 恢复/重试本地渲染任务，
  与 Office 的显式信任对话框不同，取消只影响本次等待。
  后台任务采用顶部分类、左任务列表、右详情/日志面板，两栏之间的分隔条可拖动调整列表
  宽度（钳制在可读最小宽度与详情面板最小宽度之间，窗口缩小也不会挤压详情）；
  每页 12 项，按创建时间与 ID 稳定
  排序，2 秒轮询不改变搜索和筛选。切换库范围须立即清空旧数据并使迟到查询失效，日志
  查询独立门控，不得串到其他任务。`src/job_diagnostics.rs` 将闭合事件枚举和数值指标
  写到数据目录 `job-logs/` 的独立 SQLite，不改图书数据库结构；按规范库路径和任务 ID
  隔离，每任务 500 行、全库 50000 行、文件页数限制约 64 MiB。记录不依赖窗口打开，
  日志失败不得改变任务结果；不接受任意正文、URL 或错误字符串，不逐 token 记录。
  日志读取经服务重新核验当前任务归属，时间明确为 UTC，保留清理与无历史记录须可见。
  翻译任务的「文本块明细」不止由持久游标推导：`BlockInspector` 的已处理/待处理来自
  游标（`completed` 就是 `next_ordinal`），「处理中」来自游标 JSON 里的 `inflight` 窗口集合
  —— 整本翻译会同时开多块，单看游标说不出在跑哪几块；旧格式游标、写坏的 JSON、非运行态一律
  当作「没有在飞的块」，不得凭空把游标那一块显示成处理中。失败块必须来自该任务最新一次执行的
  `ProtocolSkipped` 与带 ordinal 的 `StepFailed`
  事件（回滚式失败只把游标退回第一个未翻译块，命名不了整批失败块）。失败块从游标给的状态
  里移出，四个计数必须恒等于块总数；筛选、跳到当前进度、复制都要按同一个视图计算。
  任务失败时明细页顶部必须给出持久化的失败原因与本次未翻译的块编号，`RunFailed` 也要带
  `error_kind`，否则头部失败行只剩位置。  `JobLogErrorKind::label()` 是闭合表的固定中文说明，
  只能用于呈现，不得改写成错误正文。
  文本块明细每行的“查看”按钮打开只读调试面板（`TranslationBlockDetail`）：它**不读**任何
  持久化的请求副本——请求 body、系统提示与用户消息从不入库（见 `job_diagnostics.rs` 的闭合
  事件约定），面板按被钉住的修订**重新解析文本块并按需重建** `translation_request`。
  因此 worker（`translate_block`）与检查器必须共用同一个 `translation_prompt()`，不得各写
  一份；`translation_request` 的温度固定为 `TRANSLATION_TEMPERATURE = 0.0`，两边一起生效。
  模型名取任务的持久游标（任务执行过就以它钉住的模型为准），只有从未执行的任务才回落到
  AI 设置的 `models.translation.model`（可能为空）。读取路径与列表同为**只读**：不认领、
  不推进、不发布任务，也不写 `translations`；单块读取复用 `load_translation_blocks` 后按
  `ordinal` 线性查找，代价与列表同量级，可由用户点击触发。面板必须给正文硬高度上限并套
  内层滚动容器——GPUI 无法光栅化超出纹理上限的自然高度长文本。
  **失败块的「模型响应」页是整个窗口里唯一会真的发模型请求的地方**
  （`Indexing::probe_translation_block_response` → `TranslationBlockProbe`，服务层
  `background_job_translation_block_response`）。它存在的理由正是上面那条约定：诊断日志
  只保存固定分类，想知道模型到底答了什么就只能拿同一份冻结输入再问一次。硬约束：
  (1) 只能由用户点「重新请求一次」触发，**不得放进轮询、也不得在打开面板时自动发**，请求
  进行中按钮必须禁用；(2) 除这一次外部请求外仍然只读——不推进游标、不写 `translations`、
  不记 `JobLogEvent`，`MockMergeProvider` 用例同时断言这三条；(3) 响应**不落库、不落日志**，
  只回给窗口，与请求 body 同一待遇；(4) 用**配置的**翻译模型和 provider（即旁边「重试」
  按钮会用的那个），不是任务游标钉住的模型名——查看入口回答的是「现在的模型会答什么」；
  (5) 判定必须复用 worker 自己的 `translation::parse_response`（连同 `execution_identity`），
  不得另写一套解码规则，否则窗口说的分类会和日志里的分类不一致；(6) 收集流必须宽容
  （`collect_probe_response`）：截断、围栏、散文一律带原文返回并写明结束原因，只有
  provider/传输失败才报错，因为那时根本没有答案可显示。取消/关闭面板丢弃回答由
  `BlockDetailState::generation` 门控，与详情加载同一把守卫。
  「模型响应」只对失败块出现（`block_detail_tabs(failed)`）：译好的块没有重问的必要。
  `src/ui/background_jobs/layout_tests.rs` 以 56 个任务、500 条日志和长详情验证
  900×640 / 1180×820 下的真实 GPUI 布局、鼠标分页和滚动归零。外层横排使用
  `flex().flex_row()` 的默认 stretch；`h_flex()` 会注入 `items_center()`，不能用于
  没有独立高度约束的左右主体，否则长列表会把标题及筛选栏挤出可视区域。
  `visual_render` 一个单元可产生多页，运行时总页数未知，快照 `total=None`；仅成功后
  使用持久游标 `completed_pages` 作为总页数，不能用 `unit_ids.len()` 或内容单元计数。
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
  单调门控 `evaluate_script` 注入；打开新章后的迟到完成不得覆盖当前章。译文逐块写入，
  一章不必等整本任务结束：窗口按 2 秒轮询本书未完成翻译任务的游标 `completed`，前进时
  重新读取当前单元，并按（单元、显示方式、译文载荷）指纹跳过未变化的推送，任务结束再
  补一次读取；块级并行下 `completed` 仍按序逐块前进，只是可能一次跳好几块（在飞的块
  全部提交后一次到位），跳变是正常的。轮询查询必须走应用 I/O runtime，不得阻塞 GPUI 回调。
  显示方式有“双语 / 原文 / 译文”三种。系统配置的 `translation.preferences.v1`
  （`bilingual`/`original-only`/`translation-only`，默认 `translation-only`）只是全局默认；
  阅读窗口工具栏的三态切换写入本书自己的 settings 行
  （`db::settings::translation_display_book_key`，`delete_document` 必须随图书清掉），
  **本书的选择优先于全局**，全局变化只影响没有自己选择的图书。一旦本书有自己的选择，
  切换旁出现“跟随全局”按钮：它删掉该行、按当前全局重算并重推，使本书重新跟随系统配置。
  窗口打开时先按全局渲染，
  再异步读取本书选择（读失败保留全局，不得阻断章节）；模式只以
  `ReaderTranslations::effective` 为准，`apply_translation_display_mode` 负责重算并重新推送。
  原文模式不查询译文表：推送空 `blocks` 让前端 `clear()` 撤掉已有译文层并恢复被隐藏的
  原文（列表包装也要还原），也不再轮询任务游标；指纹包含模式标签，因此同一批译文在
  模式之间切换仍会重写页面。  前端按
  “规范化原文文本（重复文本按文档顺序消歧）”匹配正文块级元素，译文块一律标记
  `data-ngy-translation` 并插入原文之前，默认「译文在上、虚线分隔、原文在下」。
  **译文文字本身不是控件**：单击、拖选译文都只做选择与复制，不再切换该段的显示；
  「译文」模式下每段译文末尾有一个 `[data-ngy-translation-reveal]` 图标按钮
  （`button` + 内联 SVG，1em、`opacity:0.45`、`aria-label`/`title` 在
  「显示原文」与「收起原文」之间切换），点它展开/收起该段原文——它是该模式下回到
  原文的唯一入口，因此只在 `translation-only` 模式且该块可折叠时才存在。图标带 SVG
  且不含任何文字节点，`role` 只经属性表达：译文层的 `textContent` 必须仍等于它显示的
  译文（图标加 `<title>` 或字形就会跟着选区被复制）。计数因此是「译文块数 - 表格
  单元格 - 含媒体段落」；表格单元格把译文插到单元格内部且不参与切换（保持双语、没有
  图标），含媒体段落同理（不能隐藏图片）。图标是 `<button>` 而 `MEDIA` 匹配按钮，
  可折叠判定必须在插入任何 chrome 之前算出，否则会把自己排除掉。
  译文节点必须从 `annotations.js` 的 `textIndex()` 与选区文本中排除（跨译文选区按
  fragment 过滤译文后再取文本），原文文本节点始终保留在 `body`，使笔记 UTF-16 锚点、
  版本校验和重叠标记语义不受翻译影响。落在译文层内的选区**不能丢弃**：`currentSelection()`
  与 `READER_INITIALIZATION_SCRIPT` 的 `boundedSelection()` 都先调用
  `ngyTranslations.originalRange(range)`（`translations.js` 拥有该映射：记录每个译文叶子
  由哪个原文叶子重建，取选区命中叶子中首个到最后一个原文叶子的跨度），拿不到映射才返回空。
  译文与原文没有逐字符对应，粒度因此是“叶子”：单叶段落等于整段，行内 `<strong>` 等只映射
  该叶子；`choose()` 对非 AI 解释动作改用当前选区，避免点击早于 80ms 防抖时用旧快照。
  原生菜单回传的是用户看到的译文，与原文引文不可能相等，译文快照只按冻结锚点自身校验。
  工具栏动作按当前选区执行（`choose(action)`），只有原生菜单保留冻结快照
  （`choose(action, true)`），避免点击早于 80 ms 防抖时用旧快照。
  「译文」模式原文被隐藏（`display:none`），标记没有 `getClientRects()` 可画，切到双语或
  原文才会显示；笔记卡、AI 引用与宿主校验始终是原文。
  解释译文时页面把所选译文作为 `displayed_text` 回传，只允许 `ai_explain` 且不超过
  `MAX_READER_SELECTION_BYTES`（`AnnotationAction::valid()`）；`reader_explanation_reference`
  把它放进 `AiReferenceHint.displayed_text`，`ai_sidebar::selection_explanation_question`
  据此构造提问（空值回退固定问题，超长按 `MAX_QUESTION_BYTES` 在字符边界截断而不是拒绝）。
  `frozen_text`、引用快照、来源与笔记始终是原文，`displayed_text` 只进提问，不进快照、
  引用或存储。译文文字只用 `textContent`/文字节点写入，不能当 HTML。
  译文格式只从当前原文 DOM 的白名单元素与排版 computed style 重建，禁止复制 ID、事件和
  URL 属性；逐项核验源文字叶节点，匹配失败保留原文。直接列表项保留 li 和编号，内部
  原文包装必须可在 clear 时恢复；含媒体段落保持双语，不能隐藏图片。HTML/XHTML 均须支持。
  手工修改单块译文（`TranslatedBlock::manual` + 每块 `key`）：页面在每个译文叶子旁提供
  「编辑译文 / 保存 / 取消 / 恢复机器译文」（`data-ngy-translation-controls`，默认 `opacity:0`
  且 `pointer-events:none`，悬停或 `focusin` 时显现——用 `visibility:hidden` 会把按钮移出 Tab
  序列，键盘用户将无法进入编辑）。控制行自身放在该 host 的 shadow root
  （`attachShadow({mode:"open"})`）里：按钮文字是 chrome，留在译文层的 light DOM 里会跟着
  选区被复制，也会让译文层报出比它显示的译文更多的文字（`focusin` 会穿过边界，悬停由
  layer 的 `mouseenter` 决定）。编辑器只在译文叶子位置替换成单行 `contenteditable` 纯文本
  节点（`Enter` 禁止、`Escape` 取消、粘贴只取 `text/plain`、空译文在页面先拒绝），原文节点、
  笔记 UTF-16 锚点与 `originalRange()` 叶子映射都不受影响（`pair.copy` 在编辑期间就是该
  可编辑节点）。保存把 `source`（取自**原文**叶子，不是页面显示的文字）与当前文本经
  `manual_translation` IPC 交给宿主，宿主重新读取该章并强制推送；页面只有在
  `result({ok:true})` 之后才把内容落成普通文本节点，失败时保留输入并显示原因，且必须能看到
  「取消」。**后台逐块推送与编辑互斥**：同一章节（revision 相同）的载荷在编辑或保存进行中先
  缓存，编辑器关闭后再应用，绝不能用重推丢掉正在输入的文字；revision 变化（切章）立即应用。
  IPC 侧 `ManualTranslationRequest::valid()` 只做粗筛（动作、非空、片段/字节上限），权威校验在
  `set_manual_translation`：两处上限必须一致，页面与宿主都必须拒绝空译文，只有「恢复」允许
  空片段列表。
  `src/ui/reader/translations.test.cjs` 是可选 DOM 门禁（Node + 已安装 Playwright），
  覆盖双语顺序、重复文本消歧、嵌套块、单元格插入、译文末尾图标的展开/收起（含“单击译文
  不再切换”）与笔记/选区排除，
  以及手工译文用例：编辑→保存的请求形状、失败后保留输入、重推延后到取消之后、空译文不发请求、
  编辑时选区仍解析回原文（HTML/XHTML）。
  2026-09-11 格式回归：11 项 HTML/XHTML DOM 用例通过；`tests/translation_flow.rs`
  通过本机 mock SSE 验证请求、严格回填、重启、缓存失效、切端点与重译。真实 Windows
  独立 EPUB 的 14 块中文译文验证标题/强调/颜色/换行/列表/表格/上下标/代码和段内切换，
  重启仍保留译文，取消/确认退出正常，两次错误日志为空；未连接真实模型或用户书库。
  2026-09-12 手工译文：`tests/translation_flow.rs` 新用例覆盖手工译文覆盖机器文本、重启与换
  端点后仍显示、恢复后回到机器文本且不新增模型请求，并拒绝改写原文/截断/超大/未知块/空译文；
  `services` 单元用例覆盖手工译文不校验模型与执行身份、版本失效与不可解析的手工列退回机器
  文本；阅读窗口 IPC 用例覆盖动作/空值与上限拒绝，宿主的接单门
  （`manual_edit_belongs_to_current_chapter`：URL 必须解析为当前章、`revision` 必须等于本窗口
  推送过的值）由独立纯函数用例固定，没有版本记录的章推送 0 也必须接受回传的 0，否则该章
  永远不能手工改，所以不得改成「必须有版本记录」的严格比较。
  2026-09-12 `translation_flow` 偶发失败已定位，不是手工译文的回归、也不是产品侧挂起：用 6 个
  测试进程并发加压可稳定复现，桩件时间线显示 mock 在 1—2 ms 内应答，真正的成因是宿主测试
  自己的预算与轮询方式——(1) 轮询 `background_jobs_for_books` 每 20 ms 新开一个连接，和本次
  运行的写事务相撞后会跑满应用 5 秒 `busy_timeout`，报 `database is locked`（SQLITE_BUSY）；
  (2) 单个用例的 20 秒总截止与 10 秒 `wait_for_requests`/`wait_for_responses` 预算在机器过载
  时不够（同一次运行从 8 秒涨到 33 秒）。夹具已按「失败必须可归因」重构：`wait_translation`
  改为「30 秒无进展才失败」（另有 240 秒上限），两个 mock 等待预算 10 → 30 秒，mock 读超时
  5 → 30 秒且 `read_request` 返回 `io::Result`（掉线连接只记录、不再杀死桩件），轮询对
  SQLITE_BUSY 有界重试（`is_database_busy`，`open_fixture_conn` 给直接连接同样的 busy
  timeout），失败信息一律附上进程级桩件时间线（连接/请求/应答与时间戳）。用例
  `a_second_writer_reports_the_lock_conflict_the_poller_retries` 钉住重试依赖的错误形状。
  加固后 42 次 6 进程并发运行全部通过（加固前同一实验 6 个进程里 2 个失败）。该套件当前为
  17 项运行 + 1 项需真实模型的忽略用例。同一轮加压后另观察到 `cargo test --lib` 的
  `indexing::tests::vision_endpoint_switch_uses_new_provider_and_preserves_embedding_route`
  等同类紧截止失败（`src/indexing.rs` 里 5 秒的「旧 vision 请求未开始」与 `wait_for_state`
  截止，3 次重跑里 1 次）：这是该模块测试自己的预算问题——整块测试并行，有些夹具还故意给
  提供方 5 秒延迟，5 秒预算等于和它要等的工作赛跑。已把这些进展等待统一为模块内的
  `TEST_PROGRESS_WAIT`（30 秒），`wait_for_state` 的失败信息补上任务 ID 与预算；6 个 flow
  进程并发加压下连跑 3 次 lib 全部通过（每次 40—47 秒）。真正的挂起仍会失败，只是多等一会儿
  并打印卡在哪个状态。诊断同时留下一条产品侧观察：`db::connection::open_conn` 只给 5 秒
  `busy_timeout`，而 UI 的后台任务窗口与阅读窗口译文进度都是「每 2 秒新开连接」的查询，极端
  过载下这类查询可能报 `database is locked`；本次未改产品代码，需要时再评估提高耐心或减少
  连接抖动。
  2026-09-12 DOM 门禁：本机 Node 26.8.1（fnm，`E:\ai\fnm\node-versions\v26.8.1\installation`）
  加已安装的 Playwright 1.61.1（借用其它项目的包，`NODE_PATH` 指向其 `node_modules`，
  浏览器用 `%LOCALAPPDATA%\ms-playwright\chromium-1228`，不安装也不改动本仓库依赖）运行
  `node --test src/ui/reader/translations.test.cjs`：23 项全部通过。首轮 5 项失败暴露了两个
  真实问题与一个测试缺陷并已修掉——控制行按钮文字混进译文层的 `textContent`（改 shadow root）、
  XHTML fixture 没有 `xmlns` 因而元素不再具备 HTML 接口（fixture 补齐命名空间，真实章节都带它）、
  以及新用例在 `focus` 同一帧读 `opacity` 而没等淡入（改为 `waitForFunction`）。
  同一轮 `node --test src/ui/pdf_reader/annotations.test.cjs` 4 项、
  `src/ui/pdf_reader/viewer.test.cjs` 4 项通过；`src/ui/reader/annotations.test.cjs` 19 项中
  1 项失败（`clicking a mark lists its thoughts…`：`result()` 之后直接取 `.mark.highlight`，
  而同一文件第一个用例是靠 `waitForFunction` 等划线渲染的），该文件与 `annotations.js`、
  初始化桥都与 HEAD 一致，待单独定位，不要当成手工译文引入的回归。
  2026-09-12 真实模型门禁：`tests/translation_flow.rs` 的
  `a_local_model_replays_the_previously_rejected_blocks` 默认 `#[ignore]`，把现场日志里的三个
  文本块（含 inline 边界，片段切分逐字一致）交给本机模型复跑真实翻译任务，需要
  127.0.0.1:11434 上的 Ollama（`NGY_REPLAY_ENDPOINT` / `NGY_REPLAY_MODEL` / `NGY_REPLAY_LOG`
  可覆盖）。它断言持久化任务日志里不出现 `JobLogErrorKind::InvalidSchema`，并断言两个 2 段块
  必须落库；3 段的段落会被小模型合并成一段而按数量检查跳过，那是协议该做的拒绝、不得判成
  任务失败。对 `qwen3.5:0.8b` 的实测：全部拒绝都是 `segment_count_mismatch`
  （`expected_segments=2/3 actual_segments=1`），2 块落库、1 块跳过，
  `translation_run_untranslated` 而非 `run_failed`；修复前同一批块是 3/3 `invalid_schema`、
  连续 3 块未译后整本任务失败（该中止条件已于 2026-09-15 取消）。换模型后重跑该门禁即可复验，
  不要把它并入默认测试。
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
  `NGY_TEST_CHROMIUM` 可指定浏览器；不为测试安装或修改项目依赖。前者用合成多页文字层
  覆盖按页笔记、按页命中与草稿钉页；后者用测试内生成的最小 PDF 驱动提交的 `assets/pdfjs`
  产物，覆盖整本连续滚动成列、远离阅读位置的页面回收与返回重绘、`ngy-pdf-page-changed`
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
  `NGY_TEST_CHROMIUM` 可指定浏览器；不为测试安装或修改项目依赖。该门禁同时覆盖命中
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
  原生右键“AI解释”回传的是 WebView2 按 Blink 文本迭代器提取的选区文字：跨段落或
  含 `<br>` 时会插入 `\n`，而冻结范围的 `textContent` 没有任何分隔符，因此两者只能
  按可见字符比较（`compact`），不能直接比对规范化文本；`\n`、制表符或首尾空白不一致
  都会让合法选区被误判为“选中文字已变化”。锚点始终来自冻结范围，宿主仍按去空白后
  的 UTF-16 范围校验，比较放宽不会改变笔记位置。
- `web/pdf/` 与 `assets/pdfjs/`：固定版本 PDF.js shell、lockfile、清单和提交的本地
  资产；不得改为 CDN 或运行时联网获取。`web/pdf/src/viewer.mjs` 是上下连续滚动的实现：
  页列一次性布局，`IntersectionObserver` 按需绘制、远离阅读位置后回收为占位页；滚动
  停止约 150ms 才上报 `ngy-pdf-page-changed`，并复用最近一次 `ngy-pdf-go-to` 的
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
cargo run --locked --bin ngy-book-studio
cargo build --locked --bin ngy-book-studio
cargo build --release --locked --bin ngy-book-studio
```

不要使用裸的 `cargo run` 或 `cargo run --release`，否则 Cargo 无法确定要运行哪个
二进制。

### 数据目录与日志位置

数据目录在首次启动时由用户确认，不再使用 `ProjectDirs` 的默认位置；选择记录在
`%APPDATA%\ngy\ngy_book_studio\config\bootstrap.json`（实现见 `src/startup.rs`）。
设置界面是应用自己的窗口（`src/ui/data_dir_setup.rs`）：输入框里预填推荐路径或上次的
选择，可以直接改、可以点「浏览…」调系统目录选择器，点「确认并启动」（或回车）后才校验
并记录；校验在后台执行器上做，失败原因显示在同一个窗口里。关闭设置窗口即放弃启动。
日志固定在 `<数据目录>/logs/`，按 UTC 日期分文件、保留 7 天。目录写入配置之前必须
通过“建目录 + 写删探针”校验；记下的目录之后不可用时会重新询问并说明原因，不会静默
回落默认目录。系统配置页显示两个目录位置，并提供更改数据目录与打开目录的入口。

更改数据目录等于**搬迁**：设置界面选新位置后写配置并记下待搬迁的旧目录，随后自动重启
墨页；新进程在打开图书库之前把旧目录整体搬过去（同卷重命名，跨卷复制后才删源），旧目录
随之清空。搬迁失败不会静默：保留记录、把原因显示在设置窗口里，确认原位置可重试，换成
别的空目录也可以。搬迁期间没有进度界面，也不能中断（大书库跨卷复制会看到启动等待）。
目标目录里已经有一个图书库时必须先确认覆盖 —— 那会删掉它原有的内容，不可撤销；目标
非空又不含图书库时直接报错，要求换一个空目录。

自动化与人工冒烟不得选真实书库：debug 构建的 `NGY_DATA_DIR` 优先级高于配置与设置窗口，
且不写配置、不执行待办搬迁（搬迁只在本次真的要用配置里那个目录时才做）。发布构建忽略
该变量，避免普通用户被环境变量改走目录。

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
cargo test --bin ngy-book-studio --locked
cargo test --test epub_flow --locked
cargo test --test multi_format_flow --locked
cargo test --test openai_compatible_flow --locked
```

`src/ui/` 的单元测试属于产品二进制目标，不包含在 `cargo test --lib` 中。完成 Rust
改动前仍应执行全目标检查和测试；不要只用快速命令作最终验收。

### 可选 DOM 门禁（Node + 已安装 Playwright）

`src/ui/reader/translations.test.cjs`、`src/ui/reader/annotations.test.cjs`、
`src/ui/pdf_reader/annotations.test.cjs`、`src/ui/pdf_reader/viewer.test.cjs` 用
`node --test <file>` 运行。它们只使用本机已安装的 Playwright，不为测试安装依赖、
不触碰用户数据，`NGY_TEST_CHROMIUM` 可指定浏览器。改动 `src/ui/**/*.js` 的交互代码
或 `web/pdf` 产物时建议跑对应门禁，并在 `ROADMAP.md` 或本文件记下结果。

本机（2026-09-15 复核）可用组合：Node 22.22.2 用工作台自带的
`C:\Users\admin\.workbuddy\binaries\node\versions\22.22.2-3\node.exe`，Playwright 1.63.0
借自 `%TEMP%\moye-pw\node_modules`（临时目录，装 Playwright 1.63.0，里面另有 `*.cjs`
诊断脚本，被清理后需换一处已安装的包），`NODE_PATH` 指向它。**浏览器必须显式指定**：
Playwright 1.63 默认要 `chromium-1243`（本机只有 `chromium-1228`），不设
`NGY_TEST_CHROMIUM` 会在 `before` 钩子里以 `Executable doesn't exist at ...1243` 失败全部用例：

```bash
NODE_PATH='C:\Users\admin\AppData\Local\Temp\moye-pw\node_modules' \
NGY_TEST_CHROMIUM='C:\Users\admin\AppData\Local\ms-playwright\chromium_headless_shell-1228\chrome-headless-shell-win64\chrome-headless-shell.exe' \
"$NODE" --test src/ui/reader/translations.test.cjs
```

`NODE_PATH` 与 `NGY_TEST_CHROMIUM` 都要写 Windows 路径（Git Bash 的 `/c/...` 形式
Node 认不出来，`require("playwright")` 会 `MODULE_NOT_FOUND`）。

门禁失败先分清产品缺陷与用例缺陷：XHTML 夹具必须带 `xmlns`（否则元素不再具备 HTML
接口），异步重绘要用 `waitForFunction` 等，而不是在同一个任务里直接读结果。

### AI 问答诊断日志

AI 日志统一使用 `ngy_ai` target。未设置 `RUST_LOG` 时，默认
`warn,ngy_ai=info`，记录问答开始、完成、取消及失败；详细排障使用
`warn,ngy_ai=debug`。日志同时写终端和数据目录下的
`logs/ngy-book-studio.<YYYY-MM-DD>.log`（见 `src/logging.rs`），终端输出带源码文件
和行号。数据目录确定之前的启动阶段没有日志，需要整段启动过程时，在 PowerShell 中运行：

```powershell
$env:RUST_LOG = "warn,ngy_ai=debug"
$aiLog = Join-Path ([System.IO.Path]::GetTempPath()) ("ngy-ai-" + [guid]::NewGuid() + ".log")
cargo run --locked --bin ngy-book-studio 2>&1 | Tee-Object -FilePath $aiLog
```

开发验证还须按下文 GUI 冒烟要求设置唯一 `NGY_DATA_DIR`。环境变量只作用于从该
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

### 导入诊断日志

导入链路统一使用 `ngy_import` target。未设置 `RUST_LOG` 时，默认
`warn,ngy_ai=info,ngy_import=debug,ngy_reader=debug`：记录 UI 选中的路径、注册表候选与选定解析器、
各格式适配器阶段、库持久化和最终结果，Kindle 另按记录打印压缩/尾部/解压字节数。
这些日志是“界面提示导入失败但控制台为空”这类反馈的第一手依据，新增导入分支时必须
同时补齐对应阶段日志与失败日志。需要保留一次复现时：

```powershell
$env:RUST_LOG = "warn,ngy_ai=info,ngy_import=debug,ngy_reader=debug"
$importLog = Join-Path ([System.IO.Path]::GetTempPath()) ("ngy-import-" + [guid]::NewGuid() + ".log")
cargo run --locked --bin ngy-book-studio 2>&1 | Tee-Object -FilePath $importLog
```

导入路径上的日志只记录字节数、记录序号、格式、置信度、媒体类型、稳定 ID 与受控错误
分类；不记录正文、译文、选区、密钥或完整文件内容。用户可见的失败仍以界面提示为准，
日志只用于定位阶段，不能替代界面上的错误信息。

不需要 GUI 复现一次导入时，`examples/probe_import.rs` 会走同一格式层与
`LibraryStore` 路径，把库落到临时目录并在其后重新打开，用于区分“解析失败”与
“持久化/回读失败”。它不写真实数据目录，但也不覆盖界面状态机与文件对话框。

### 阅读器诊断日志

阅读器资源供给统一使用 `ngy_reader` target，未设置 `RUST_LOG` 时同样默认开启 debug：
每章打印请求路径、媒体类型、正文字节数、被改写的命名实体名与数量；每份二进制资源打印
状态码、字节数与 Range；`export.rs` 生成阅读器投影时每章打印一次实体改写汇总。
“章节打开后是渲染错误页而不是正文”这类反馈先看这里——它只报告实体名和计数，不含正文。

因为 WebView 只给出 “Entity 'xxx' not defined” 和行号，脱离 GUI 定位这类问题用
`examples/probe_reader.rs`：它导入到临时库、走 `LibraryStore::reader_epub_bytes`
生成投影，再用 `reader::load_resource` 取出 WebView 真正会收到的字节，按行/列报告
XML 解析不了的命名实体。

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
uv run --locked python -m ngy_lab compare --scenario all
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

桌面从课程 `.venv/Scripts/python.exe -I -u ngy_lab/desktop_host.py` 启动受信宿主，
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
$env:NGY_FORMAT_CORPUS = "C:\path\to\ngy-format-corpus"
cargo test --test format_corpus_gate --locked -- --ignored --nocapture
Remove-Item Env:NGY_FORMAT_CORPUS
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
  同一 key 还保存后台任务调度（`concurrency` 1–1024 默认 1、`interval_ms` 0–60000 默认 10），
  读取时钳制而不是报错，坏行不得让图书库无法打开；两者不属于 Provider JSON，保存后经
  `configure_scheduling` 立即作用于后续任务，正在运行的任务不被打断。
- PDF 紧凑阅读使用独立 settings key（`pdf.reader.preferences.v1`，默认关闭，AI 设置
  “系统配置”中的“PDF 紧凑阅读”默认不勾选），同样不得扩展 Provider JSON；新 key 必须
  加入 `AI_SETTINGS_KEYS`，否则 `snapshot_ai_settings`/`restore_ai_settings` 的失败回滚
  会不对称（`restore_ai_settings` 校验备份行数）。页间距只有一份契约：宿主用
  `ngypdf://viewer/viewer.html?...&compact=1` 让新窗口在首帧前设置
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
- 默认显示语言使用独立 settings key（`translation.preferences.v1`，AI 设置
  “系统配置”中的下拉默认选“中文（简体）”，预设中/英/日/韩/法/德/西/俄等，也提供“不翻译
  （仅原文）”），同样
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

Debug 构建支持 `NGY_DATA_DIR`；Release 构建忽略它并访问真实 LocalAppData。设置了它
的进程不弹数据目录设置窗口、也不写 `bootstrap.json`、不执行待办搬迁；未设置时首次启动
会打开该窗口（输入框预填推荐路径，确认后才启动）。人工 GUI 验证必须使用唯一隔离目录 ——
尤其别在隔离环境里去点「更改数据目录」，那会往真实配置里写搬迁记录。

全目标测试会因 dev-dependency 合并 GPUI 的 `test-support` 特性；不要直接用测试
命令留下的产品 EXE 作最终 GUI 验收。先单独执行产品构建
`cargo build --locked --bin ngy-book-studio` 或下方的产品 `cargo run`。
GPUI 0.2.2 的测试执行器在真实 Windows
dispatcher 上会直接拒绝带超时的等待，从而在退出时产生
`timed out waiting on app_will_quit`；不能未经正常产品构建复测就将它认定为应用退出故障。

```powershell
$env:NGY_DATA_DIR = Join-Path ([System.IO.Path]::GetTempPath()) ("ngy-agent-" + [guid]::NewGuid())
$env:RUST_LOG = "error"
cargo run --locked --bin ngy-book-studio
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
