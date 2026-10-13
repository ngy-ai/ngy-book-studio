# 搜索、视觉与 AI 安全约束

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

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
  「每分钟请求数上限」（`EndpointSettings::requests_per_minute`，0＝不限，上限
  `MAX_ENDPOINT_RPM`）同样是端点级设置，与超时一起校验，读取时钳制而不是报错；
  额度按规范化 `base_url` 在进程里共享（`ai::rate_limit`），只在请求**发出之前**等待，
  不得为了让某个任务更快而绕过或放宽它 —— 绕过就等于把服务商的 429 变回一次立刻重发的
  请求。排队不计入请求超时，交互请求（问答、Agent、检测模型，以及问答链路里检索用的
  查询向量 —— 它与后台索引共用 embedding 端点的额度，但类别是交互）优先于后台任务；
  429 按 `Retry-After` 给整个端点写冷却，「上限不限」也不得跳过冷却。
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
- 翻译生成参数（温度、Top P、最大输出、两种惩罚）存于自己的 settings key
  （`ai.openai_compatible.translation_generation.v1`），与问答参数**分开存储**：取值范围
  一致，默认值不同（翻译默认温度 0、上限 4096，问答温度 0.1）。新 key 必须加入
  `AI_SETTINGS_KEYS`，否则失败保存的回滚会留下半新半旧的行。翻译参数**不属于翻译执行
  身份**（`translation_execution_identity` 只由翻译端点与翻译模型派生）：改它不重建任务、
  不把游标退回书首、不影响已存译文，因此保存时不需要任何确认；缺行时按翻译默认值补齐，
  升级不改变老库发出的请求。该参数随每次翻译请求（含协议纠正与逐片段回退）发送，温度
  留空等于请求体里不带这个键。
- 后台任务自动运行配置使用独立 settings key，默认关闭（`DEFAULT_AUTO_RUN_BACKGROUND_JOBS`
  = false，AI 设置中“自动运行新创建的后台任务”默认不勾选），不得扩展 Provider JSON。
  默认或关闭时，新导入、创建或保存编辑仍须在文档事务内创建 `visual_render`、`vision`、
  `embedding` 三类任务，但初始状态为 `Paused` 且未开始；已有任务不随设置切换改变状态，
  用户仍可在后台任务窗口逐项恢复。
  同一 key 还保存后台任务调度（`concurrency` 1–64 默认 1、`interval_ms` 0–60000 默认 10），
  读取时钳制而不是报错，坏行不得让图书库无法打开；两者不属于 Provider JSON，保存后经
  `configure_scheduling` 立即作用于后续任务，正在运行的任务不被打断。
- PDF 紧凑阅读使用独立 settings key（`pdf.reader.preferences.v1`，默认关闭，AI 设置的
  「系统配置」窗口中的“PDF 紧凑阅读”默认不勾选），同样不得扩展 Provider JSON；新 key 必须
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
- 默认显示语言使用独立 settings key（`translation.preferences.v1`，AI 设置的
  「系统配置」窗口中的下拉默认选“中文（简体）”，预设中/英/日/韩/法/德/西/俄等，也提供“不翻译
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
- 删除一轮问答（界面的「删除」）先经 `ChatScope` 授权与本窗口自己会话的校验，再在一个
  事务里删除问题与其回答、重排后续 `ordinal` 并把被删父节点的子消息挂回前驱；锚点不是
  本会话的问题、或指向别的窗口重写过的线程时一行都不写，界面按「已不在存储会话中」
  报告。控制器只有在存储侧删除成功后重新加载并发布转录，存储失败不得让界面显示一条
  并未删除的记录。