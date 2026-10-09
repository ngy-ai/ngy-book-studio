# 代码地图：阅读窗口、笔记与译文对照

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 模块

- `src/ui/reader/annotations.rs`、`annotations.js`：Reader 笔记宿主与可信选区菜单，
  闭合 Shadow DOM 隔离正文 CSS，覆盖层绘制而不改写正文节点。
  `src/ui/reader/selection_menu.rs` 是 EPUB 与 PDF 共用的 WebView2 原生「AI解释」菜单，
  由调用方提供私有文档判定与事件构造，仍要求 page/frame 为同一私有文档。
- `src/ui/reader/speech.rs`、`read_aloud.js`：Reader 朗读（Windows 系统语音）。
  页面按当前显示模式收集段落（有译文层就读译文）并发起朗读，宿主合成播放并把当前
  段落推回页面高亮；切章、切显示方式与重新加载都会停止，进程内只允许一个朗读会话。
  详见 `docs/agent/speech-and-read-aloud.md`。
- `src/ui/reader/translations.rs`、`translations.js`：Reader 双语对照展示。宿主在
  `sync_loaded_page` 之后按当前书/单元从   `translations` 表读取与当前
  `(document_revision, unit_revision)`、翻译模型与翻译执行身份一致的译文，经 session/generation
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
  **存下的分段形状属于写入时的章节结构**：来源被重新导入后，阅读投影可能并掉一层内联
  包装（自定义内联元素在规范化时被并回文字），同一段文字在页面里只剩一个叶子，而译文行
  仍按旧的三个叶子分段。这种**更细的分段按边界拼回当前叶子**：每个存下的分段必须完整
  落在某个叶子里、按顺序铺满整块且不剩分段，拼回的译文写进该叶子，该叶子的原文就是这些
  分段的拼接（手工保存按同一形状提交，宿主按同一套铺满规则接受）。分段要跨叶子切开
  （当前叶子比存下的分段更细）无法把译文切回原文，仍按失效处理并保留原文。拼接不会掩盖
  文本变化：拼接结果必须与叶子逐字相同，文本一变队列键就对不上。
  **译文文字本身不是控件**：单击、拖选译文都只做选择与复制，不再切换该段的显示；
  「译文」模式下每段译文末尾有一个 `[data-ngy-translation-reveal]` 图标按钮
  （`button` + 内联 SVG，1em、`opacity:0.45`、`aria-label`/`title` 在
  「显示原文」与「收起原文」之间切换），点它展开/收起该段原文——它是该模式下回到
  原文的唯一入口，因此只在 `translation-only` 模式且该块可折叠时才存在。图标带 SVG
  且不含任何文字节点，`role` 只经属性表达：译文层的 `textContent` 必须仍等于它显示的
  译文（图标加 `<title>` 或字形就会跟着选区被复制）。计数因此是「译文块数 - 含媒体
  段落」；含媒体段落保持双语且没有图标（不能隐藏图片）。图标是 `<button>` 而 `MEDIA`
  匹配按钮，可折叠判定必须在插入任何 chrome 之前算出，否则会把自己排除掉。
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
  URL 属性；逐项核验源文字叶节点，匹配失败保留原文。列表项与表格单元格这类就地插入的块
  保留 li/td/th 本身（标记与格子不能换），把原文子节点移进 `display:contents` 的包装再
  隐藏包装——隐藏元素本身会连译文一起隐藏；包装必须可在 clear 时恢复。含媒体段落保持
  双语，不能隐藏图片。HTML/XHTML 均须支持。
  手工修改单块译文（`TranslatedBlock::manual` + 每块 `key`）：页面在每个译文叶子旁提供
  「编辑译文 / 重新翻译 / 保存 / 取消 / 恢复机器译文」（`data-ngy-translation-controls`，默认 `opacity:0`
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
  空片段列表。宿主按「请求与机器行整段铺满」对齐：等长时一一对应，请求更粗（每个请求片段覆盖
  ≥1 个完整机器片段）时按顺序拼接，落库的手工行保持请求的形状、原文一律取自机器行；切开
  机器片段、改写原文或留下没覆盖的片段都按「原文结构已改变」拒绝。
  **逐块「重新翻译」**（2026-09-22 起）是控制行上的第三个按钮，走另一条 IPC
  （`retranslate_block`，载荷只有 `revision / request_id / key`；`RetranslateBlockRequest::valid()`
  只拒绝空标识、越界标识与请求序号 0）。页面只说得出**块标识**：序号由宿主在任务自己的文本块
  列表里查出来（`IndexingCoordinator::retranslate_translation_block`），页面多送一个 `ordinal`
  也不会被采用。提交期间行内显示「正在提交重新翻译…」，成功后留一句「已提交重新翻译，完成后
  本章自动更新」—— 新译文要等后台任务跑完那一块才会入库，本章由 2 秒轮询重新推送，所以宿主
  **不立即重载**（那只会把旧文本再推一遍）；失败时显示宿主给的原因，按钮回到原位可以再点。
  两个约束要记住：① 手工译文优先于机器文本，重跑只改机器行，读者自己写的文字不会消失，要看到
  新机器文本得先「恢复机器译文」；② 任务正在运行时重试会被拒绝（`reset_for_translation_block_retry`
  不打断执行器），页面如实显示这句原因，用户先暂停任务再点即可。
  `src/ui/reader/translations.test.cjs` 是可选 DOM 门禁（Node + 已安装 Playwright），
  覆盖双语顺序、重复文本消歧、嵌套块、单元格插入、译文末尾图标的展开/收起（含“单击译文
  不再切换”）与笔记/选区排除，
  以及手工译文用例：编辑→保存的请求形状、失败后保留输入、重推延后到取消之后、空译文不发请求、
  编辑时选区仍解析回原文（HTML/XHTML）；逐块重新翻译用例：按钮只提交块标识与修订、提交期间
  行内忙碌、成功后留下提示且不改手工标记、失败后显示原因并可再点；更细分段拼回当前叶子的用例
  （HTML/XHTML）：拼接后的译文、图标与控制行、拼不回时保留原文、拼接块的手工请求形状。
  2026-09-23 分段拼接：来源被重新导入后，阅读投影会并掉译文行拆分时的内联包装（现场
  《AI Engineering from Scratch V5》第 14 章的自定义内联元素 `<box>`：译文行是三个分段，
  页面里只剩一个叶子），整块译文被静默丢弃——既没有译文，也没有图标和控制行。前端改为把
  能按边界铺满叶子的更细分段拼回该叶子，宿主按同一套「整段铺满」接受手工请求。
  `node --test src/ui/reader/translations.test.cjs` 26 项通过（含新增 2 项），
  `cargo test --all-targets --locked --no-fail-fast` 通过（lib 里
  `translation_walk_test::a_translation_run_keeps_the_configured_number_of_blocks_in_flight`
  在整块并行下偶发失败、单项重跑通过，属该模块紧截止夹具的既有波动）；本轮未做人工 GUI
  验证，未在真实窗口打开该书该章。
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
  `ai::indexing::tests::vision_endpoint_switch_uses_new_provider_and_preserves_embedding_route`
  等同类紧截止失败（`src/ai/indexing/` 里 5 秒的「旧 vision 请求未开始」与 `wait_for_state`
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