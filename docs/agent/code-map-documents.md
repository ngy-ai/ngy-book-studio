# 代码地图：统一模型、格式、存储与图书库

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 模块

- `src/document/`：稳定 ID 的格式无关模型，包括 `BookDocument`、`ContentUnit`、
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
- `src/markup/`、`src/editing.rs`、`src/export.rs`：HTML 解析清洗、事务式
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
- `src/library/`：SQLite 与对象存储之上的图书库业务编排、导入/创建/保存/删除、
  垃圾回收和兼容现有 UI 的投影。
  打开图书库时按顺序做两件一次性修复：先 `run_startup_blob_gc` 回收未引用对象，
  再 `run_startup_language_backfill` 把导入器漏记的声明语言补回 `books.language`。
  回填只读容器元数据（`formats::declared_language`，EPUB 取 OPF 的 `dc:language`），
  不碰正文、不覆盖已有语言、不动 `revision` 与 `updated_at`，因此索引、译文、笔记与
  排序都不受影响；完成标记 `library.language_backfill.v1` 写进 `settings`，让重读原件
  只发生一次（原件可能很大，不能每次启动都读）。空库不写标记——没有书可查就不该
  提前用掉这次机会；有条目读不出来或解析失败也不写标记，留给下次启动重试。
  `import_into_group` 是图书库界面的导入入口：`group_id` 只作用于本次插入的书，
  由 `build_persisted_graph` 写进同一条 `books` 行（`previous` 的分组优先，保存与
  编辑因此不会顺带移动图书）；内容哈希命中的 `AlreadyExists` 不移动既有图书。
  界面在 `ui/library/state.rs::import_target_group` 里只把“停在某个具体分组”翻成
  目标分组，“全部图书/未分组”返回 `None`；分组已从图书库消失时同样返回 `None`，
  不让一次本可成功的导入因为分组校验失败（`import_into_group` 收到不存在的分组会
  直接报错）。
  改名与改作者（`rename_book` / `set_book_author`）只写 `books` 的 title/author 一行
  （`update_catalog_metadata`），**不**推进文档版本：`BookDocument` 每次读取都从该行
  取标题，规范投影本来就一致。走 `apply_document` 会为了一个字符串重导整本 EPUB、重读
  并重哈希每个资源（含可能上 GB 的字节一致原件）、删掉并重建全书检索分块、丢掉全部
  派生索引/视觉任务并重排整本译文。其中分块重建最致命：FTS 删除触发器按
  `search_chunk_id`（FTS 的 UNINDEXED 列）逐行删除，每行都要全表扫描，块数上万时单次
  改名实际不会结束（2026-09-23 现场：9 302 块的书改名耗时 75 s～110 s，元数据单行更新
  后 12 ms）。因此元数据变更必须保持单行更新，`rename_book_only_updates_the_catalog_row`
  用源版本/内容单元/分块/FTS/派生任务的行身份断言这一点。
  同一个 FTS 陷阱对**任何**重发表文档的路径仍然存在：`install_document_revision` 会删掉
  前一来源的全部分块再重建，编辑保存、封面替换等在有大量分块的书上同样会非常慢。彻底
  修法是把删除改成按 FTS rowid 定位（触发器 DDL 变更 + `SCHEMA_VERSION` 递增，会按重建
  策略清空现有书库），当前没有做，出现“保存一本书很久没反应”时先看这个原因。
- `src/annotations.rs`、`src/db/annotations.rs`：三种划线、人工想法与 AI 想法统一使用
  `annotations` 一张表。宿主按实际阅读章节 body 文本（排除 script/style/noscript/template，
  移除 ECMAScript 空白）的 UTF-16 起止位置校验 quote、书/单元归属及打开时的版本。
  修订或删除章节保留失效笔记，不按 quote 搜索重定位；删除图书级联清除笔记。
  同书/单元/双版本/精确起止范围的三种标记由部分唯一索引约束，改样式在事务中替换，
  不创建多条标记。删除划线只删除该范围的标记，人工与 AI 想法保持不变。
  当前开发结构版本为 14，遵循重建策略，不编写迁移。
- `src/share/`、`src/library/share.rs`、`src/db/transactions/share.rs`：朋友间分享用的
  `.ngyshare` 包，一个文件同时带一本书和它的笔记、AI 问答会话与整本译文。
  `share/` 只负责格式：`manifest.json` + `book.epub`（规范化阅读投影）+ `notes.json` /
  `chats.json` / `translations.json`，`read_share_package` 逐条限额，条目数与清单不符、
  版本不受支持或不是 ZIP 都明确拒绝；`is_share_package` 只读清单做内容签名判定，导入
  对话框据此路由，扩展名不参与判断。
  **分区条目只按「章节序号 + 原文引用」定位，绝不写发送方的图书、内容单元或文本块 ID**：
  book id 由导入原件的内容哈希派生、unit id 由 book id + 序号 + href 派生、
  `{unit}::h{index}` 的块标识又由 unit id 派生，接收方重新导入同一份文本也必然得到
  另一套 ID。因此导入按序重锚：笔记用接收方阅读章节的正文 UTF-16 范围复验
  （`reader_body_text` + `validate_anchor`），会话引用重建为章级 `DocumentLocator`
  并丢掉发送方的 `search_chunk_id`（接收方索引是本地派生的），译文按接收方
  `{unit}::h{index}` 形状重挂；对不上的条目逐条跳过并在结果里计数，不按引文搜索重定位。
  只导出当前可显示的数据（失效笔记、过期译文、内部 system/tool 消息不写入），
  笔记用 `insert_if_absent`、会话用确定性 ID 跳过已存在的行，所以重复导入不会产生重复。
  **接收方已在同一选区划的线不会被分享包改色**：同锚点已有划线时只在样式相同时按无操作
  成功，样式不同按跳过处理，绝不把别人的书签改成分享方的样式。
  译文按接收方当前翻译模型与 `translation_execution_identity` 重新署名（
  `AppServices::share_translation_stamp`），否则阅读端按模型/身份过滤会把分享来的机器译文
  全部隐藏；接收方未配置翻译模型时保留发送方身份并等设置对齐。分享包以规范化 EPUB 投影
  为载体，因此 PDF/DjVu 页面笔记（锚在 PDF.js 页面文字层上）无法在接收方章节正文上复验，
  会被跳过。
- `src/db/`：SQLite 连接、当前结构、单表 CRUD/查询映射和跨表事务。每张表对应一个
  文件：`books.rs`、`book_sources.rs`、`content_units.rs`、`toc_entries.rs`、
  `blobs.rs`、`assets.rs`、`asset_refs.rs`、`progress.rs`、`search_chunks.rs`、
  `embeddings.rs`、`index_jobs.rs`、`visual_pages.rs`、`visual_page_staging.rs`、
  `chat_threads.rs`、`chat_messages.rs`、`chat_citations.rs`、`groups.rs`、`settings.rs`、
  `translations.rs` 和
  `office_enhancements.rs`；跨表原子操作只放 `transactions.rs`，FTS 查询放
  `book_search.rs`，建表与完整性契约放 `schema.rs`。