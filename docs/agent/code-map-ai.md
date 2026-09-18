# 代码地图：AI、检索与翻译

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 模块

- `src/ai/`：AI 后端包。`mod.rs` 是 OpenAI-compatible 模型访问，`agent.rs`/`agent_runtime.rs`/
  `agent_chat.rs`/`chat.rs` 是只读问答 Agent 与会话持久化，`search.rs`/`indexing/` 是检索与
  索引/翻译后台任务，`translation.rs`/`credentials.rs`/`web_search.rs`/`ai_diagnostics.rs`/
  `job_diagnostics.rs` 是分段协议、端点密钥、联网补充与诊断。
- `src/ai/search.rs`、`src/ai/indexing/`：作用域内 FTS5/`sqlite-vec` 精确 KNN、RRF 混合
  召回，以及可恢复的 embedding/vision 后台任务。`indexing/` 按职责分文件，不要把任何一个
  文件重新堆回大杂烩：`mod.rs` 是协调器、游标、共享类型与常量，`worker.rs` 是任务循环与
  「一轮派发一个任务」的判定，`embedding.rs`/`vision.rs` 是两个模型通道，`control.rs` 是
  暂停/取消观察与结果发布，`inspection.rs` 是只读的任务窗口块检查器，`support.rs` 是共用
  小工具，`translation/` 是整本翻译任务（`mod.rs` 走位与提交、`blocks.rs` 取块、
  `prompt.rs` 请求体、`response.rs` 应答收集与校验）。`tests/mod.rs` 只放共用夹具与可计数的
  mock 提供方，用例按主题分文件：`unit`（纯请求/应答形状）、`embedding`、`job_control`、
  `endpoints`、`vision`、`scheduling`、`translation`、`translation_retry`、
  `translation_walk`；新增用例放进对应主题的文件，不要再往夹具文件里塞用例。
  子模块一律用 `use super::*;` 取得上面的共享项，跨模块引用按 `pub(crate)`（测试夹具用
  `pub(super)`）收口；协调器
  的 `impl` 块可以分散在 `mod.rs` 与 `inspection.rs`。执行器固定启动
  `MAX_BACKGROUND_JOB_CONCURRENCY` 个 worker，只有序号小于当前并发的 worker 扫描队列，
  每个任务提交后按配置间隔休眠；两者由 AI 设置的「后台任务」窗口经
  `IndexingCoordinator::configure_scheduling` 实时发布，写坏的值按范围钳制。
  同一个「任务并发」既是任务上限，也是**单个整本翻译的块窗口上限**：一轮里最多同时开
  `concurrency` 个文本块请求（`walk_translation_blocks` 的窗口，每轮重读设置，所以运行中
  调大只让 walk 走得更前、不打断任何在飞请求），因此一本翻译自己也会出现多个「处理中」。
  `ai/indexing/translation/` 另实现整本图书翻译任务
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
  `src/ai/translation.rs` 定义只含文字的分段协议：每次提供整段上下文与整数 ID，模型必须
  返回完整、唯一的 ID 集合；按原文顺序回排并恢复片段边界空白，不能用模型 HTML 替换正文。
  每个请求的用户 JSON 还带 `book`（书名）与 `chapter`（章节标题）两个语境字段，供模型消歧：
  它们从冻结的文本块取（`TranslationBlock.book_title` / `unit_title`，经
  `TranslationContext`；`validate_translation_source` 按含上下文的那一份算请求上限），与正文
  走同一条不可信数据通道，空标题整段省略、超长标题按 `MAX_CONTEXT_LABEL_CHARS` 截断，绝不出现在
  片段列表或译文里。工作线程与任务窗口都必须从文本块取上下文，两边请求体才逐字节一致；
  上下文只进用户消息，不写进系统提示。
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
  冻结输入的开关，并且现在也覆盖流失败。收流还有一条自己的**静默**上限
  （`TRANSLATION_STREAM_SILENCE_LIMIT`，与 `PROVIDER_STEP_TIMEOUT` 同值）：provider 层的
  静默计时兜不住「流不再产生任何事件」——现场 2026-09-17 的整本翻译在开出 8 个块请求后
  彻底静默（job-logs 无写入、进程没有任何 TCP 连接、`await_translation_step` 的 180 秒
  也没触发），于是整本书永久停在同一块上，并占住 `transitions` 门闩把 AI 设置保存一起
  卡住。兜底按 `ProviderTimeout::stream` 报错（分类仍是 `http_timeout`，不新增类别），
  让任务走出来而不是无限等待；**总时长仍然不设上限**，持续产出数据的慢模型继续能翻完。
  流没有正常结束（含 `ProviderTimeout` 静默超时）
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
  比送进模型更安全。**改动这套规则必须把 `ai::indexing::TRANSLATION_BLOCKS_VERSION` 加一**：
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
- `src/ai/mod.rs`、`src/ai/credentials.rs`：OpenAI-compatible models/chat streaming/embeddings
  接口、端点策略和 Windows Credential Manager 密钥存储。
  端点配置的“请求超时”不再作为 reqwest 的整段请求超时：`Client` 只保留 10 秒连接超时，
  非流式调用（models/embeddings）由 `within_request_timeout` 按总时长约束，流式回答由
  `ProviderTimeout::stream` 约束“两个数据块之间的静默”，因此持续输出数据的慢模型不会被
  中途掐断（2026-09-11：120 秒整段超时切掉了一条仍有 1579 个正文块、7024 字节正文、
  没有 `[DONE]` 的流，整本图书被判失败）。流的总规模仍由 `max_tokens`、32 MiB 字节上限和
  65536 事件上限约束。`ProviderTimeout` 在 `ai::ai_diagnostics` 中映射为固定分类
  `http_timeout`，后台任务日志与 UI 不因超时改在块间生效而出现新类别。
  `SseDiagnostics` 每 10 秒输出一条与内容无关的传输进度行（`sse_progress`：字节数、事件数、
  正文块数、空块数、距上次事件的时间、最大间隔），结束时不再只给分类计数。
  **AI 与联网搜索的客户端默认跟随系统代理**（reqwest 自己读 `HTTP_PROXY`/`HTTPS_PROXY`），
  而默认端点正是本机 Ollama，代理会把它一并接管；「系统配置」的「使用系统代理」开关
  （`ai.network.preferences.v1`，默认开）关掉后走 `OpenAiHttpProvider::new_with_proxy` /
  `HttpWebSearch::new_with_proxy` 的 `.no_proxy()` 客户端。代理是系统级偏好，因此**不进
  `ProviderConfig`**（endpoint 级结构，且被大量测试字面量构造）；两处客户端都只在保存设置
  时重建，开关对已发出的请求无效。
- `src/ai/agent.rs`、`src/ai/agent_runtime.rs`、`src/ai/agent_chat.rs`、`src/ai/chat.rs`：只读 Agent
  工具、SSE/tool-call 循环、窗口授权范围、会话/消息/引用持久化与对话编排。