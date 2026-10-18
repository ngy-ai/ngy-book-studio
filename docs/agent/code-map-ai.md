# 代码地图：AI、检索与翻译

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 模块

- `src/ai/`：AI 后端包。`mod.rs` 是 OpenAI-compatible 模型访问，`agent.rs`/`agent_runtime.rs`/
  `agent_chat.rs`/`chat.rs` 是只读问答 Agent 与会话持久化，`search.rs`/`indexing/` 是检索与
  索引/翻译后台任务，`translation/`/`credentials.rs`/`web_search.rs`/`ai_diagnostics.rs`/
  `job_diagnostics.rs` 是分段协议、端点密钥、联网补充与诊断。
- `src/ai/translation/`：分段协议按职责分文件，不要重新堆回一个文件：`protocol.rs`（格式说明与
  两个请求体构造：单块 `request_input`、批 `batch_request_input`）、`decode.rs`（找答案：围栏/
  思考块/裸片段列表）、`parse.rs`（`parse_response`、`restore_segments` 与字符分类）、
  `repair.rs`（一次结构标点归一）、`error.rs`（`ResponseError` 分类）、`segment.rs`（落库形状）、
  `source.rs`（`TranslationSource` 与 `TranslationContext`）、`batch.rs`（批信封、打包预算与
  逐块解析）。`tests/` 按主题分文件（`mod.rs` 只放共用夹具与包装用例，其余是 `repair_test`、
  `segment_test`、`batch_test`），子模块用 `use super::*;`。
- `src/ai/rate_limit.rs`：按 Endpoint 的每分钟请求数上限（RPM）。`EndpointRateLimits` 是
  `AppServices` 生命周期的注册表，键为规范化后的 `base_url`，因此同一台服务器上的问答、
  翻译、向量、视觉与模型检测共用同一份额度（provider 会随保存设置整批重建，额度不能挂在
  provider 上）；`EndpointRateLimits::limit_for` 是只读视图（不会顺手建立条目），诊断与测试
  用它确认保存后每条地址上生效的上限。`EndpointLimiter` 是滑动窗口闸门：窗口里还有位置就
  直接放行，满了就在**发出请求之前**等待（`OpenAiHttpProvider::await_endpoint_budget`，位置在
  `within_request_timeout` 之外，排队不计入请求超时）；交互请求（`RequestClass::Interactive`：
  侧栏问答、Agent、检测模型，以及问答链路里检索用的查询向量）排在后台任务（`Background`：
  翻译、向量索引、视觉）前面，两者共用额度，只有排队顺序不同。装配时检索走的是同一端点上的
  另一个 provider 实例（`build_ai_services` 的 `search_provider`）：`EndpointRateLimits::slot`
  按地址返回同一个 `EndpointLimiter`，所以「同一份额度、两种类别」是一条不变量，
  `endpoint_budget()` 把它暴露给装配测试。等待者带 drop guard：请求被取消时把自己的号从队里
  带走，否则队首会卡死整条队（`acquire` 返回之后它已是空操作）。服务商返回 429 时按
  `Retry-After`（只认秒数，读不到用 60 秒，钳制在 1 秒–10 分钟）给整个端点写下冷却；
  `acquire` 的快速路径必须同时看冷却 —— 上限「不限」不等于可以硬撞已经给出的 429。上限存在
  端点自己的设置里（`EndpointSettings::requests_per_minute`：`0`（含缺键的老行）＝「自动」，
  由 `default_endpoint_requests_per_minute` 按地址解析成具体数字 —— 回环端点不限（本机没有
  服务商额度，只有 GPU 负担），远程端点按 `DEFAULT_ENDPOINT_RPM`＝10；正数是固定上限）。
  读它的地方必须走 `EndpointSettings::resolved_requests_per_minute`（装配 provider、
  `publish_endpoint_limits`、模型检测），保存后立即发布，读取时钳到 `MAX_ENDPOINT_RPM`。
- `src/ai/search.rs`、`src/ai/indexing/`：作用域内 FTS5/`sqlite-vec` 精确 KNN、RRF 混合
  召回，以及可恢复的 embedding/vision 后台任务。`indexing/` 按职责分文件，不要把任何一个
  文件重新堆回大杂烩：`mod.rs` 是协调器、游标、共享类型与常量，`worker.rs` 是任务循环与
  「一轮派发一个任务」的判定，`embedding.rs`/`vision.rs` 是两个模型通道，`control.rs` 是
  暂停/取消观察与结果发布，`inspection.rs` 是只读的任务窗口块检查器，`support.rs` 是共用
  小工具，`translation/` 是整本翻译任务（`mod.rs` 走位与提交、`blocks.rs` 取块、
  `prompt.rs` 请求体、`response.rs` 应答收集与校验）。`tests/mod.rs` 只放共用夹具与可计数的
  mock 提供方，  用例按主题分文件：`unit`（纯请求/应答形状）、`embedding`、`job_control`、
  `endpoints`、`vision`、`scheduling`、`translation`、`translation_retry`、
  `translation_walk`、`reconcile`（对账一步一个单位、让路与静默看门狗）；
  新增用例放进对应主题的文件，不要再往夹具文件里塞用例。
  子模块一律用 `use super::*;` 取得上面的共享项，跨模块引用按 `pub(crate)`（测试夹具用
  `pub(super)`）收口；协调器
  的 `impl` 块可以分散在 `mod.rs` 与 `inspection.rs`。
  库每次写入后经 `wake()` 置位 `reconcile_translations`，worker 把翻译任务对账
  **按「一步一个单位」** 推进（2026-10-15）：`advance_translation_reconcile` 每轮只应用
  一个单位（一条任务的插入、重置或取消，各自一个小事务，见
  `db::transactions::apply_translation_reconcile_unit`），每步之前重新扫描队列；队首排着
  **本计划之外**的任务时这一步让路（`Yielded`），worker 先去跑它。让路不消费对账请求
  （外部任务跑完继续推进），也不会被对账自己刚插入的任务拖住 —— 给它们让路等于让一次
  对账等整本书翻完，其余单位就再也建不出来。全量入口
  `db::transactions::reconfigure_translation_jobs`（保存设置、启动）与分步入口共用同一份
  计划（`plan_translation_reconcile`）与判定，只差不等 worker 的下一轮。
  「已领取但无进展」由独立的静默看门狗报告：`watchdog_loop` 随协调器启动，每
  `NO_PROGRESS_WATCHDOG_INTERVAL`（= 30 秒）检查一次；任务被领取后超过
  `NO_PROGRESS_WATCHDOG_THRESHOLD`（= 300 秒，容纳单发 180 秒流静默上限加 60 秒冷却）
  没有任何任务日志事件时，写一条 `ClaimedNoProgress`（带无进展时长与当前游标）。
  它只报告、不取消不重排；事件本身走 `record_index_event`，因此会刷新观察时间戳，
  同一段无进展每个阈值周期最多报一条。
  **暂停的结清点（2026-10-17）**：`pause_requested` 只是一个「待观察」标志，只有拥有游标的
  执行者能把它消费成 `paused`。因此 `db::index_jobs::request_pause` 对**排队中**的行当场
  结清（直接写成 `paused` 并清掉标志）—— 排队中的任务没有执行者，只留标志的话这行既领不走
  （`claim` 要求 `pause_requested = 0`）也没有人消费，任务窗口会永远显示「正在暂停」且
  `available_actions` 一个操作都不给（现场：整本翻译点暂停后一直暂停中，重启仍旧）。
  `recover_interrupted` / `recover_interrupted_kind`（启动恢复）把同一个标志一并结清：
  被恢复的 `running` 行带着标志就按 `paused` 落，旧构建留下的「排队中 + 标志」行同样修好，
  其它状态上的残留标志只清不掉状态。运行中的行照旧只置标志，由执行者按它最后持久化的
  游标发布 `paused`（发布 JSON 必须与库里那份逐字相同，见下文游标规则）。
  执行器固定启动
  `MAX_BACKGROUND_JOB_CONCURRENCY` 个 worker，只有序号小于当前并发的 worker 扫描队列；
  并发上限与间隔由 AI 设置的「后台任务」窗口经
  `IndexingCoordinator::configure_scheduling` 实时发布，写坏的值按范围钳制。
  「任务间隔」有两个作用点：① 任务粒度 —— `worker_loop` 在每个任务提交后休眠一次（视觉与
  向量任务靠它节流）；② **请求粒度** —— 整本翻译的每个并发槽位在自己的 `translate_block_batch`
  收尾后、接下批之前再休眠一次（`pause_after_translation_request`，停止请求不休眠；一发批请求
  里的多块共用这一次休息，缓存命中与「不必处理」的块不发请求也就不休眠）。
  只有②能约束一本书内部的请求频率：一整本书是一个任务，①对它等于不存在（现场 2026-09-20：
  本机模型连续请求把 GPU 吃到满载、桌面黑屏，用户把间隔调到 5000 毫秒也不起作用）。
  同一个「任务并发」既是任务上限，也是**单个整本翻译的请求窗口上限**：一轮里最多同时开
  `concurrency` 发请求，每发带走几块由翻译端点的「每请求文本块数」决定（攒批在 `walk.rs`、
  打包预算在 `batch.rs`），所以在飞的文本块最多是「任务并发 × 每请求块数」
  （`walk_translation_blocks` 的窗口，每轮重读设置，所以运行中调大只让 walk 走得更前、不打断
  任何在飞请求），因此一本翻译自己也会出现多个「处理中」。
  `ai/indexing/translation/` 另实现整本图书翻译任务
  `kind="translation"`：任务标识为 `translation:<source_id>:<target_language>`，游标用
  `next_ordinal` 记录「第一块尚未提交的文本块」，并按序调用**翻译模型** `chat_stream` 写入
  `translations` 表，可暂停/恢复/重试/取消；文本块按 `content_units.block_json` 的 `BlockDocument` 确定性
  提取（段落、标题、引用、列表项、表格单元格；代码保持原样），以
  `(document_revision, unit_revision, target_language, 翻译模型)` 判定失效并重译。目标语言
  或翻译模型变化由 `AppServices::configure_translations` 经
  `transactions::reconfigure_translation_jobs` 重排；每本当前来源只保留一个目标语言任务。
  源语言（`books.language` 主语言子标签）等于目标语言时跳过；翻译任务未配置翻译模型时
  失败而不猜测。模型调用不得逐 token 打日志，也不得记录正文或译文内容。
  **问答模型与翻译模型是两个独立设置**（`ProviderSettings.chat_model` /
  `translation_model`，端点绑定 `EndpointRoutingSettings.chat_endpoint_id` /
  `translation_endpoint_id`，各自是 `ModelRole::Chat` / `ModelRole::Translation`）。
  翻译执行身份 `translation_execution_identity` 只由**翻译端点 + 翻译模型**派生，
  `AiServices.translation_provider` 也按翻译端点构建，因此换问答模型或问答端点都不会让
  已翻完的图书重翻；阅读端按 `translations.model == translation_model` 过滤。
  两个字段是后加的键：`load_provider_settings` 读不到 `translation_model` /
  `translation_endpoint_id` 时按当时保存的问答模型与问答端点补齐，所以升级不会改变既有
  翻译执行身份、也不会触发整本重译。
  **保存设置后，正在跑的整本翻译每一轮都重读 `models.translation`**
  （`walk::refreshed_translation_services`）：同引擎下换 provider 或换生成参数（改 Endpoint
  密钥、超时；温度等）从下一块起生效，不必重启任务，已翻好的块照旧复用 —— 现场 2026-09-24：
  改完 Endpoint 与 API key 之后，正在跑的任务用领取时的快照把旧值用到书末。只有模型、执行
  身份与目标语言都一致才允许换：那三项里任何一项变了都是一次会重置任务行的重配置（改地址、
  换模型、换目标语言），本次运行会在下一个控制检查里被判成已被替换（`Abandoned`），重新领取
  时才按新引擎继续，绝不能半路换身份、把两种来源的译文混进同一次运行。改地址或换模型后任务
  回到配置的初始状态（`translation_initial_status`：自动运行开着就排队，否则暂停等待恢复），
  已存译文经 `adopt_execution_identity` 重盖身份后继续复用。
  **同一套规则也适用于 embedding 与 vision 的运行**：`embedding.rs` 每一批、`vision/run.rs`
  每一页都重读 `IndexingInner::models_snapshot()`（`embedding_provider_refreshed` /
  `vision_provider_refreshed` 记一条日志），引擎一致（embedding 另加维度）就换用新的
  `embedding_provider` / `vision_provider`（换 key、超时、代理等从下一批/下一页起生效），
  不一致则保持领取时的快照，由 `reconfigure` 的任务行重置接手。两条运行都只换 provider：
  模型、维度与执行身份始终取自领取时的快照，落库的模型列与执行身份因此不会在一次运行里漂移。
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
  `src/ai/translation/` 定义只含文字的分段协议：每次提供整段上下文与整数 ID，模型必须
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
  答案、截断 JSON、缺失/重复/未知 ID 均拒绝。片段对象只有 `id`、`text` 和**可选的** `keep`
  三个字段。不需要翻译的片段（纯符号、数字、公式、代码）必须由模型显式声明：
  `{"id":0,"text":"","keep":true}`，该片段整段保持原文（首尾空白一并保留）。`keep` 只能与
  留空的 `text` 同时出现：同时给出译文按 `keep_with_text` 拒绝，留空 `text` 又不声明 `keep`
  仍按 `empty_segment_text` 拒绝 —— 「没写」不等于「确认过」。现场 2026-09-20（`qwen3:0.6b`）：
  模型对纯符号片段留空回答，块因此反复重试后被判未翻译，用户要求改为「模型确认过这段不需要
  翻译」才接受。提示词侧必须与解码器逐条对齐，且**每条规则只写一处**：`keep` 的写法只出现在格式
  说明里（普通项就是 `id`+`text` 两个字段，只有 `keep` 项才多一个字段），纠正提示复用同一套说法；
  「保持原文」是应用行为，提示词只描述可判定的写法（`text` 恰好是空字符串），否则弱模型会把原文抄
  进 `text` 而落到 `keep_with_text`（现场 2026-09-21 block 589：`188M trained`）。数字与专有名词
  只要求「本身不改写」，与「整段没有需要翻译的文字才写 `keep`」划清边界，避免「保留数字」与
  「数字片段不翻译」给出相反动作。片段首尾与相邻片段之间的空白由 `parse_response` 从原文补回，
  提示词不再要求模型保留，只说「由应用补回，不需要你处理」。
  解析失败时只允许一次**结构标点归一**：把字符串字面量之外的全角结构字符（`：，｛｝［］＂“”`）
  换成半角 ASCII 后再走同一条严格流水线。它只改写 JSON 里只可能是结构的位置，因此合法
  响应永不进入该路径、译文内容逐字不变；键名吞掉分隔符（现场 266 块 `"text："`）要恢复
  只能猜模型意图，必须继续拒绝并保留原始终止行列。格式说明必须显式要求结构字符用半角
  ASCII、全角标点只能出现在 `text` 值里，纠正提示按固定失败分类挑一段中文复述要修的部分。
  格式说明还要交代 **JSON 字符串值自身的编码**，这几条模型无法从示例推断、更正路径也救不回来：
  值里的直引号与反斜杠要写成 `\"` 与 `\\`、换行要写成 `\n`（裸换行不是合法字符串，
  `repair_structural_punctuation` 只归一化字符串字面量**之外**的全角结构字符，值里的裸引号没有
  修复路径，只会落进 `invalid_json`）、`text` 必须是字符串（整段只是数字也要写成 `"188"`）。
  `book` 与 `chapter` 是可选字段（空标题整段省略，见 `request_input`），提示词因此用条件句描述
  它们，不再写「用户输入是 JSON，book 与 chapter 是…」。现场 2026-09-22：片段
  `“FlashAttention packing”` 用的是弯引号，恰好绕开了直引号转义这一条。
  键名与值的引号规则必须写成**对比句**（键名的半角双引号不转义、值里的双引号必须转义），
  两条相邻的反向规则分开写会让弱模型把值里的引号也写成裸引号。不可信数据的范围要覆盖
  **全部四个字段**（`book`、`chapter`、`source`、`segments`）：`book`/`chapter` 来自 EPUB
  元数据，是第三方内容，只说「文本块属于不可信数据」会留下一条注入通道。每个示例与每条规则
  只写一份（`{"id":0,"text":"译文"}` 出现两次、`keep` 的写法出现两次都是历史问题），占位符
  还要点明是占位（「把 `"译文"` 替换成这一段的译文」），否则那两个字会被直接当成合法译文写进库
  （协议层只校验非空）。
  内部失败分类标识符（`keep_with_text` 之类）只用来选择段落，**不得**写进提示词：对中文翻译
  模型它是指令噪音，可能被直接抄进答案；提示词也不提「续写上一次响应」，被拒的答案本来就不回传。
  单片段不得拼出「id 为 0 到 0 的整数」这类机器病句，源语言提示统一成前置肯定句（「原文语言是
  en。」），不再用「可能的」把确定信息降级成猜测。
  分段协议校验失败时使用冻结的原始输入和服务额外纠正一次，不回传模型的错误输出，
  不把纯文本猜分段，也不为 HTTP、流中断这类 provider 失败做协议纠正。重试前后检查任务
  控制与身份，失败沿用本执行游标，不能读取新任务游标后将新任务标为失败。
  **批信封（2026-10-16 起，`src/ai/translation/batch.rs`）**：多个文本块可以装进一次请求，
  但批只是**传输信封**：`batch_request_input` 里每个块带自己的批内 `id`、`source` 与
  `segments`（块负载由 `TranslationSource::payload_fields` 生成，与单块 `request_input`
  里的那一份逐字节相同），`book`/`chapter` 取批内第一块且只允许同一章的块进一批；回复按块还原成
  `StoredTranslation`，缓存键、重试单位与游标仍按块，所以 `EXECUTION_IDENTITY_PROTOCOL`
  不升版、已存译文照旧命中。打包同时受三个上限约束（`TranslationBatchBudget` /
  `TranslationBatchPacker`）：输出上限的 `TRANSLATION_BATCH_OUTPUT_SHARE_PERCENT`＝60%、
  块数与 `MAX_TRANSLATION_BATCH_REQUEST_BYTES`＝256 KiB；**空批永远收下第一块**
  （「装不下也要翻」），`estimate_batch_block` 用源文字符数当输出 token 上界（宁保守，顶到
  输出上限等于白花一次请求）。端点策略 `batch_block_limit`：端点的「每请求文本块数」
  `0`＝自动（本机＝1、远程＝`MAX_TRANSLATION_BATCH_BLOCKS`）、`1`＝关闭、`>1` 为上限并钳到
  `MAX_TRANSLATION_BATCH_BLOCKS_SETTING`。`parse_batch_response` 把**信封**错误
  （`batch_block_count_mismatch`、`batch_block_ids`，以及不是批形状时的 `invalid_schema`、
  `ambiguous_json`、`incomplete_json`）整批退回单块路径由调用方拆半重发，**块自己的**片段形状
  错误（`segment_count_mismatch`、`keep_with_text` 等）只拒绝那一块、同批其它块照常落库；
  批这一层不重试、不纠正，宽容解码只做单块路径上那几件必要的事。**调用方是 walk**
  （2026-10-16 起）：`TranslationServices::batch_blocks`（端点「每请求文本块数」的解析结果，
  见 `EndpointSettings::resolved_blocks_per_request`）大于 1 时 `walk.rs` 按同一个内容单元
  攒批、受上面三个上限约束，窗口仍按**请求**计（同时在飞的块最多「任务并发 × 每请求块数」）；
  批里每一块照旧落回它自己的块号，拆半重发、块级回退单块与整发失败逐块跳过都由
  `response/batch.rs` 内部完成，`TranslationRunStats::batched_requests`/`batched_blocks` 只记
  walk 打包的那几发（拆半重发的几发不进这项读数）。回归用例：协议形状在
  `ai::translation::tests::batch_test`，调用方语义在 `ai::indexing::tests::translation_batch_test`
  （按批成包与逐块归位、坏块回退单块、坏批拆半、截断一直拆到单块、整发失败逐块跳过、
  跨章不混批、收窄重试只发目标块）。
  **块级重发（2026-09-15 起，`TRANSLATION_BLOCK_RETRIES = 5`）**：一个文本块的一轮失败后，
  整块按同一份冻结输入重发，最多 5 次，所以一个块最多 6 轮（一轮 = 两次严格尝试 + 一次协议
  纠正）。重发轮的第一发带着上一轮的失败分类，因此重发不是逐字相同的请求；每轮开始时先在
  同一把 `transitions` 门闩内复查控制请求与来源版本。**只有协议类失败（`ResponseError`）才
  重发**：provider、数据库与取消错误立即上抛让整次运行失败，重发同一个请求解决不了它们。
  失败分类跨轮累积（`rejected_kinds` 属于整块而不是某一轮），否则轮 1 的
  `segment_count_mismatch` 会被后面轮次的 `incomplete_json` 顶掉，最后一轮就不肯走逐片段
  回退。逐片段回退只留给最后一轮：它是整块的最后手段，前面几轮只做严格尝试与纠正，免得一个
  多片段块把请求预算耗在重复的逐片段请求上。翻译的采样参数由设置给出（`TranslationGenerationSettings`，
  默认温度 0、输出上限 4096，与这条路径一直以来的取值相同），所以**默认**配置下同一份请求
  重发在确定性模型上会拿到逐字相同的答案（现场：块 3579/3620 在两次运行里的响应字节数
  完全相同），重发真正救回的是截断与采样抖动这类非确定性失败；要救「确定性不合格」的块，
  只能靠失败分类不同的重发提示，而不是重发次数 —— 用户把温度调高后，这条「重发拿到同一答案」
  的前提自然不再成立，前提只在默认值上成立。
  纠正在两次请求后仍失败的文本块保留原文并继续翻译其余块：按块记 `ProtocolSkipped` 警告（块序号、总数、固定分类），
  并像缓存命中一样持久推进游标（执行器要求内存游标与持久游标始终一致）。**跳过没有连续上限，
  也没有「整本图书没有任何有效译文」的中止条件**：跳过多少块都要一直走到书末，跳过本身按
  `RunOutcome::Succeeded` 收尾，游标永不回滚（2026-09-15 起按用户要求「失败了就跳过，直到整本
  书的文本块都运行完」；原 `MAX_CONSECUTIVE_TRANSLATION_SKIPS` 常量与
  `publish_untranslated_failure` 回滚入口已删除）。
  **请求级失败按同一方式跳过，不管连续多少个都不停**：传输、超时、流协议、上下文超限（以及「响应超过
  大小上限」这类响应级错误）只在那一块上记 `ProviderSkipped` 并继续翻后面的块。个别块连不上很常见，
  为它废掉整本书是错的（现场 2026-09-22：一块撞上端点抖动，整本翻译立刻失败，已经翻好的部分停在半路）；
  「连续若干块都失败就停」同样是错的（现场 2026-09-24：任务在连续 5 块失败后按 `Failed` 收尾，已处理
  3045 块、剩下 4921 块再也没人翻 —— 原 `TRANSLATION_CONSECUTIVE_REQUEST_FAILURES` 中止条件已删除）。
  `TRANSLATION_REQUEST_FAILURE_STREAK_LIMIT`（= 5）不是中止条件：连续失败凑满它时发一条
  `translation_request_failure_streak` 警告并**冷却** `TRANSLATION_REQUEST_FAILURE_COOLDOWN`
  （= 60 秒，2026-09-24 用户要求）再继续，然后把连续计数清零：下一组连续失败重新累计、再冷却一次。
  冷却同时往任务日志写一条 `Cooldown`（2026-10-15）：任务窗口里要能看出任务是在等端点缓过来，
  而不是看起来死了；书末那次按设计直接跳过、不等待的冷却不写这条。
  冷却在提交循环之外进行（不持有 `transitions` 门闩），按 `CONTROL_POLL_INTERVAL` 醒来复查控制请求，
  暂停/取消/换版本照常立刻生效；冷却期间一个请求都不发，端点恢复后剩下的块照常翻；书末已经没有
  新请求要发时那次冷却直接跳过（`frontier >= total`），不让任务在最后一分钟里白等。运行照旧走到
  书末，任务窗口里的「失败 N」与「重试失败块」是修复路径。分类在 `block_skip_cause`：数据库、IO 与取消类
  错误**不**跳过，照旧让整次运行失败 —— 跳过它们只会把损坏的结果铺满整本书。两条跳过事件都要让这一块
  进文本块明细的失败列表（`BlockFailures::from_logs` 同时认 `ProtocolSkipped` 与 `ProviderSkipped`），
  用户点「重试失败块」就是修复路径。
  **块级并行（2026-09-15 起）**：一次整本翻译按「任务并发」同时打开多个文本块请求，窗口用
  `JobCursor.inflight`（升序块号）记录，只有整本翻译会填它，其它任务恒空。三条规则必须同时
  成立：① 提交严格按 ordinal 顺序（`resolved: BTreeMap` 只在 `next_ordinal` 处出队，
  `TranslationRun::commit` 先 `next_ordinal = ordinal + 1` 再 `close_inflight`），因此
  `next_ordinal` 永不越过还在等的块，被中断的运行只会重做、绝不会把没翻完的块当成已完成；
  ② 控制检查与游标写入必须在同一个 `transitions` 门闩内（否则一次按块重试重置出来的新游标
  会被这一轮的旧游标覆盖），但等 provider 响应时绝不持锁；③ 暂停/取消/换版本/被替换一律先
  `settle()` 清空窗口并落盘，再按**清空后的**游标发布结果 —— 发布里的 JSON 必须与库里那份
  逐字相同，否则 `finalize_running_from_cursor` 匹配不上，任务卡在「处理中」；`Abandoned`
  一个字节都不写。
  写入本身被数据库拒绝时（`TranslationRun::persist` 返回错误），运行必须先把内存游标**回滚
  到最后一次成功落盘的那一份**再上抛：库里没接受过的那份游标不只是丢一次更新，它还会让失败
  发布同样匹配不上，任务于是永远停在「运行中」并带着一个幽灵在飞块（`translation_flow::`
  `failed_cursor_publication_settles_the_owned_execution_and_retry_reuses_saved_output` 用
  `BEFORE UPDATE OF cursor_json` 触发器固定这条）。回滚后的失败按最后一次提交的位置发布，
  「重试」因此只重做没提交的那一块，已保存的译文仍按缓存命中复用。
  窗口的退出条件是「没有在飞请求**且**没有待提交槽位」（`in_flight.is_empty() &&
  resolved.is_empty()`）：一轮里的块全部命中缓存、或全部不需要翻译（代码块、没有文字叶）时
  不会开出任何请求，但 `resolved` 里仍压着要按序提交的块。只按「没有在飞请求」退出会让这次
  运行不提交任何块就按 `Succeeded` 收尾、游标停在原处 —— 任务窗口的「已处理」正是从游标读出
  来的，于是显示完成而一个文本块都没处理（现场 2026-09-18：当时模型/端点变化会把游标退回书首
  重扫，开头几块早已有译文，任务点「恢复」后一会儿就完成）。重扫本身是对的，逐块提交的缓存命中
  会把已存译文重挂到当前块 ID，不需要任何模型请求。**换翻译模型不再退回书首**（2026-09-22 起）：
  重配置保留 `next_ordinal`，并且必须把它出处的 `blocks_version` 一起搬运，见
  `db::transactions::translation_cursor_json`。块请求不借用游标：它拿到的是 `TranslationBlockScope`（块号、revision、
  运行最后持久化的 JSON），每次控制轮询拿它与库里那一行比对，所以窗口前进不会被误判成
  「这一行已经不属于我」。
  被跳过的块靠明细里的两条重试路径修复，都必须复用同一个 worker 与同一套跳过规则：页顶
  「重新翻译」前的「重试失败块（N）」重跑本次全部未翻译块，块行内「重试」只重跑该块
  （`retry_translation_blocks(job, vec![ordinal])`，**不得顺带重发其它失败块**）。
  阅读端的「重新翻译」按钮是第三条入口，走 `retranslate_translation_block`：阅读端只说得出
  **块标识**，所以那里先用与 worker 同一份提取规则枚举当前修订的文本块，把标识换成序号再交给
  `retry_translation_blocks`；标识找不到（正文已改动、块被合并）就报错，**绝不换一块重跑** ——
  那会写下另一块的译文。与前两条入口不同，它要求**已有译文也重发**（用户对译文不满意、或换了
  更好的模型想重做这一块，2026-10-15）：`TranslationRange::Retry { force: true }` 写进游标的
  `force_retry`，命中的缓存因此不让路，新答案覆盖旧的机器行（手工行照旧不动）；任务窗口的
  「重试失败块」保持 `false` —— 那些块本来就没有译文，命中缓存是「别为同一块重复付费」的保护。
  运行中的任务会被 `reset_for_translation_block_retry` 拒绝（执行器拥有游标），所以服务入口先走
  `pause_running_translation` 等「已暂停」落盘再规划（与「立即翻译本章」同一条路径；这一等落在
  应用 I/O runtime 上，不能在 GPUI 回调里直接用 `tokio::time::sleep`）；`running` 之外，
  `queued` 也被接受（还没有执行器拥有这条游标，现在写下的收窄就是下一次领取读到的那一份，
  2026-10-15）。这条路径不打断执行器，也不动其它块。实现方式是把
  目标序号写进游标 `retry_ordinals` 并把 `next_ordinal` 退回最小的目标序号，让 worker 只走这一段：
  非目标块照常推进游标但不发请求。**重试集合原样交给 walk，不做前置过滤**：判定「这一块是否
  还需要请求」必须与运行的缓存规则完全一致（单元内容键 + 模型 + 执行身份），按 ordinal 猜
  「已有译文」会在两种情形下猜错并让整次重试一个请求都不发（现场 2026-09-22：重试失败块时日志里
  既没有请求体也没有响应）——阅读端其实看不到的行、以及块列表变过之后 ordinal 已经错位的行。
  已有译文的块因此按缓存命中重挂（不覆盖已有译文或人工修订），两个例外：本次收窄的强制重发
  （见上），以及「模型把原文原样抄回来」的行（`StoredTranslation::is_passthrough`：拼起来逐字
  相同，忽略空白）—— 后者不算译文，walk 继续走请求路径重发并记一条 `SourceEchoed`（它不进
  明细的失败列表，见 `code-map-ui.md`）；从 `paused` 起始的翻译任务也要能重试（新任务默认
  暂停），这条走独立的
  `reset_for_translation_block_retry`，不得放宽通用的 `reset_terminal`（视觉替换依赖它拒绝
  活跃任务）。
  收窄只到**最后一个目标**为止，两条边界不能松：① 最后一个目标提交的那一刻必须把
  `retry_ordinals` 从持久游标里清掉（`TranslationRun::commit`；`force_retry` 与它同生共死，
  块列表版本变化导致收窄作废时同样清掉）—— 留着这份收窄，以后每一次恢复或重跑都会被永久收窄成
  「只跑这几块」，而 walk 会把其余块当不必处理一路推过去；留着一个过期的 `force_retry`，整本
  剩下的块都会被强制重发；② 范围之外
  只允许缓存命中（命中就照常重挂），撞上「没有译文、又不在重试范围」的块由
  `BlockPlan::Untouched` 停下运行并按 **`Paused`** 发布，绝不按 `Succeeded` 收尾 —— 跳过
  一个没有译文的块等于把它记成已处理。现场 2026-09-20：第 70 块 provider 500 失败后用户
  只重试这一块，那次运行把 71—18168 全部推了过去，任务显示「已完成 / 已处理 18168」，而
  库里只有 71 行译文，阅读端整本没有译文可显示；现在它停在 71 并按「已暂停」收尾，用户点
  「恢复」时集合已清空，那一次按整本书继续翻。
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
  `invalid_segment_text`、`keep_with_text`、`positional_segments`）：JSON 层面的失败说明模型
  没写出答案，换请求形状无用；片段数超过 `MAX_TRANSLATION_FRAGMENT_FALLBACKS` 的块也不回退，
  直接沿用跳过规则。
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
  输出 / 容器被打断”，不含任何正文；`NGY_DUMP_TRANSLATION_RAW` 仍是唯一会打印**完整**
  响应与请求正文的开关，并且现在也覆盖流失败、**每一个被采用的答案**
  （`reason = accepted` / `fragment_accepted`，与拒绝路径同一对
  `translation_request_dump` + `translation_response_dump`）以及**传输层失败**
  （连接被拒、端点 4xx/5xx、流建立之前到期的超时：请求体照旧打
  `translation_request_dump`、`reason` 换成固定错误分类；端点 4xx/5xx 的服务端错误封套
  同时作为 `response=` 进 `translation_response_dump`，它正是「端点答了什么」）——「这一块真的翻过吗」只能靠
  这一对原始字节回答，聚合计数证明不了（现场 2026-09-20：任务显示「已处理 18168」，库里
  只有 71 行译文）。反过来说，**没有转储不等于开关失效**：重试范围外的块走 `BlockPlan::Pass`、
  内容与已有译文相同的块走缓存命中（本次收窄里标了强制重发的那一块除外，它不命中缓存，见上），
  这些路径一个请求都不发；重试本身不再有前置过滤
  （`translation_retry_plan` 记 `requested` / `next_ordinal` / `last_ordinal` / `force`，集合原样
  交给 walk），所以「点了重试但一个请求都没有」只可能是缓存命中、`Pass`、或传输层失败。
  `translation_request_dump` 打的是 `body=`，由传输层自己的
  `chat_request_body` 构造，因此是**完整**的 chat-completions 请求体（系统提示 + 用户消息 +
  模型与生成参数 + `stream`/`stream_options`），不是只把用户消息那一小段再抄一遍；
  `translation_response_dump` 打 `response=`，是模型原样回答。两处共用同一个构造函数，
  转储才等于网线上的字节。两条转储行都带 `endpoint=`：这一发**实际**发给的端点来源
  （`scheme://host[:port]`，由 `endpoint_origin_label` 从 provider 自己的 `base_url` 现取，
  因此运行中保存设置换端点后下一发立刻改标新地址；路径不进日志，与 `ai_http` span 同一条
  规则）。翻译执行身份只是哈希，同一个模型名绑在多个端点上时，只有这个字段能说明转储行
  属于谁；mock 等不标注地址的实现打 `endpoint=-`。开关默认关闭，产品运行永远不打开。
  不必开这个开关也有线索：`translation_raw_output`（`debug` 级）在每一次失败的尝试上留
  一份原始内容预览，`origin` 说明字节来自哪里 —— `stream`（已收到的模型流，含半截 JSON）、
  `http_error_body`（端点 4xx/5xx 返回的错误封套）、`rejected`（没通过格式校验的完整答案）；
  同一行还有 `endpoint`（这一发实际发给的端点来源，与转储行同一套
  `endpoint_origin_label`；多端点或中途保存过设置的现场靠它定位是哪个端点在拒绝/超时，
  mock 等没有地址的实现打 `endpoint=-`）、`body_bytes`、`truncated` 与 `body`（超过
  `RAW_OUTPUT_PREVIEW_BYTES`＝4 KB 截断）。失败路径的另外几条诊断
  （`translation_stream` 的 `interrupted`/`failed`、`translation_stream_silent`、
  `translation_response_salvaged`）同样带 `endpoint=`。默认过滤（`ngy_ai=info`）不记录
  `translation_raw_output` —— 内容可能回显正文，只有把 `ngy_ai` 调到 `debug` 排障时才输出，
  完整原文仍以转储为准。收流还有一条自己的**静默**上限
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
  退回书首重扫（已存译文仍按缓存键命中，重扫不花请求）。反过来，**不改这套规则的路径必须把
  这个版本原样搬运**：重配置（换翻译模型/翻译 Endpoint）重建翻译游标时位置要留、它的出处也要
  留（`PersistedIndexCursor::blocks_version`）。写成缺省值等于宣布「这个位置来自另一套规则」，
  一次换模型就会被执行器读成块列表变过而退回书首（现场 2026-09-22：暂停在三千多块的任务换模型
  后「已处理」归零，从 0 重扫）。
  文本叶节点必须去掉 ECMAScript `\s`
  和 Unicode `Cf` 格式字符后仍有可见字符，才能成为翻译槽：EPUB 常用 ZWSP 缩进代码行，
  ZWSP 不属于 `\s`，送进协议后模型只会回空白，`empty_segment_text` 会拒绝该文本块
  （现场 96 号代码行）。`markup` 与 `translations.js` 必须共用同一份字符表，两边叶节点
  数量不一致会让回填整体对齐失败而保留原文。该规则只会减少槽位，命中旧缓存的块最多回退
  原文，因此不提升 `translation-v2` 身份、不重译整库。源文本超过既有上限明确失败，不能
  截断后存成完整块。`translated_text` 保存校验后的 `StoredTranslation` JSON 和执行身份。
  阅读窗口可以手工改写单个文本块的译文，它存在同一行的可空 `translations.manual_text`
  （同样是 `StoredTranslation` JSON，分段形状可以与机器行不同，见下），与机器文本**并排**：
  `upsert`、缓存命中重挂与重排事务只替换机器列，必须保留手工列
  （重排确实要作废时走 `delete_for_retranslation`，它同样保留仍可显示的手工行），因此后续模型
  运行、重译、重启或换端点都不会覆盖读者写下的文字，「恢复机器译文」只是把该列写回 `NULL`。写入只走 `AppServices::set_manual_translation`：来源永远取自表内片段
  （`manual_translation_segments`，请求只能替换译文文本），并按
  book/unit/block/language/document_revision/unit_revision 精确匹配，匹配不到或校验失败即报错
  且不落库。请求的分段形状可以与机器行不同：来源被重新导入后，阅读投影会并掉旧行拆分时的
  内联包装，页面于是拿着更少的叶子来保存；请求只要按顺序**整段铺满**机器片段（每个请求片段
  覆盖 ≥1 个完整机器片段，且没有剩余）即接受，落库的手工行保持请求的形状、原文仍逐字取自
  机器行。读取时手工译文**不校验模型与执行身份**（读者自己的文字不能因换模型而消失），但
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
  `ConversationQuestion::truncate_from` 是界面的「修改提问 / 重新生成回答」入口：
  `ask_inner` 在加载历史之后、写入新问题之前调用 `ChatRepository::truncate_from`
  删除锚点消息及其后的所有消息（引用经 `chat_citations` 级联在同一 DELETE 里移除），
  替换的问答对因此占同一位置而不是再追加一份。锚点已不存在或属于别的会话（另一个
  窗口重写过该线程）时记 `stale_anchor` 警告并保留原历史，绝不猜位置；
  `ConversationAnswer::user_message_id` 与 `stored_message.id` 回给界面，供之后的
  编辑/重新生成定位。`truncate_from` 只作用于本窗口自己的会话：宿主范围仍先经
  `ChatScope` 授权，截断不改变权限判定。
  `ChatRepository::delete_question_and_answer` 是界面「删除这条问答」的存储侧：删除
  该问题与其回答（引用随 `chat_citations` 级联），把后续消息重新挂到问题的前驱并
  重排序号，使 `ordinal` 保持从零连续的序列（新消息按当前长度取号），一个事务完成；
  锚点不是本会话的问题时一行都不写。会话层（`AgentConversation::delete_turn`）在
  删除前完成 scope 校验与锚点检查，返回重新加载后的会话；删掉整个会话仍是另一条
  路径（`AgentConversation::delete_session`），两者粒度不同，不要混用。