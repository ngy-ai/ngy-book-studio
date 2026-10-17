# 代码地图：GPUI 界面

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 模块

- `src/ui/ai_settings/`：AI 设置的**五个独立窗口**（`Endpoint`、`模型设置`、`联网搜索`、
  `后台任务`、`系统配置`）；工具栏“AI 设置”是选择窗口的菜单，五个窗口可同时打开。
  `mod.rs` 放五窗口共用的身份（`AiSettingsSection`）、应用级单例登记
  （`SettingsWindowTrackers`，一条登记只服务一个窗口）、状态与提示、统一外框
  （`settings_frame`）与统一提交（`commit_settings`）；`endpoint.rs`、`models/`、
  `web_search.rs`、`background_jobs.rs`、`system.rs` 各自持有草稿、校验与面板；
  `endpoint_draft.rs` 是 Endpoint 与模型设置两个窗口共用的 Endpoint 草稿与模型检测。
  `models/` 里四个模型（问答、翻译、Embedding、视觉）**用一个 Tab 条切换**，一次只渲染
  当前 Tab 的模型区域；问答与翻译各带一组生成参数（两份设置分开存储、分别随各自的请求
  发送），因此生成参数面板只在对应的 Tab 里出现，向量与视觉没有。Tab 用分段按钮实现而不是
  `gpui_kit::component::tab::TabBar`：`TabBar` 的子项不暴露 `debug_selector`，而本仓库的 GUI
  冒烟与 GPUI 测试都按选择器定位控件。Tab 的选择器是 `ai-models-tab-<role>`；`debug_bounds`
  不清理旧帧的选择器，因此测试只能断言「新 Tab 的控件出现」，不能靠它断言旧 Tab 消失。
  **每个窗口只保存自己那一块**：各窗口的 `entered_settings` 一律以
  `services.provider_settings()` 的当前值为底、只覆盖本窗口字段，然后交给
  `AppServices::configure_providers`（唯一的提交入口），因此任何窗口的保存都不会把别的
  窗口里还没提交的输入写进数据库（改动前是一个窗口、五个标签、一次统一保存全部配置）。
  新增设置项时必须同时决定它归哪个窗口，并保持「只覆盖本窗口字段」这条约定；跨窗口共享
  的草稿只读展示，不要在窗口之间互相写入。唯一的例外是 Endpoint 列表：它由「Endpoint」
  窗口维护，模型设置窗口只在打开窗口时读一次快照，因此那个窗口重新获得焦点时要按已保存
  设置刷新这份列表（`endpoint_draft.rs::refresh_endpoint_drafts`，同一个 id 且取值未变则
  原样保留草稿与检测结果，变了或新增的重建、消失的移除），否则那边保存的新 Endpoint 在
  模型设置里选不到；刷新只动列表，失效的角色绑定退回默认 Endpoint 并给提示。模型检测在
  Endpoint 与模型设置两个窗口都可用，
  结果按 Endpoint 标识、`discovery_generation` 与规范化地址三重校验，换地址即作废；
  删除仍被模型绑定的 Endpoint 会被拒绝（以已保存设置的绑定为准，窗口快照兜底）。保存流程
  的两条上限（`SAVE_WAIT_LIMIT` 与 `AI_SETTINGS_GATE_TIMEOUT`）对五个窗口同样适用。
  窗口内的调试选择器统一由 `AiSettingsSection::selector` 生成：`ai-<窗口短名>-<name>`，
  例如 `ai-models-save-settings`、`ai-background-job-scheduling`；GUI 冒烟与 GPUI 测试
  按它定位元素，不要再写死旧窗口的选择器。底部提示条（`widgets.rs::render_notice`）
  的文字容器是横向布局的主轴子项，自动最小宽度按整段文字计算，必须显式
  `min_w(px(0.))` 才能收缩到可用宽度；模型检测失败会带回整条请求地址，缺了它提示会
  撑出窗口而不是折行（`ai-<窗口>-notice-text` 上有长提示折行的 GPUI 回归测试）。
- `src/ui/`：按窗口/职责拆分的 GPUI 界面。`library/`、`reader/`、`pdf_reader/`、
  `editor/`、`page_image/` 分别管理对应窗口；`ai_sidebar/`、
  `ai_controller/`、`ai_settings/` 管理 AI 交互；`read_aloud.rs` 是三个阅读窗口共享的
  朗读宿主层（会话状态机、进程级单会话登记与段落校验，以及朗读语速的预设档位
  `SPEECH_RATE_PRESETS` 与显示文案 `speech_rate_label`）；
  `background_jobs/` 管理当前图书范围
  的派生任务；后台任务窗口全应用只有一个，重复打开时激活现有窗口并按新的库范围刷新，
  不打开第二个；`mod.rs` 只保留跨窗口主题、窗口打开与安全关闭基础设施，其中包含按
  图书登记的窗口表：删除图书后关闭该书已打开的阅读、PDF/Office 预览、DjVu 页面和编辑窗口。
  `page_image/` 是 Office 增强预览与 DjVu 共用的页面图片窗口，以
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
  查询独立门控，不得串到其他任务。`src/ai/job_diagnostics.rs` 将闭合事件枚举和数值指标
  写到数据目录 `job-logs/` 的独立 SQLite，不改图书数据库结构；按规范库路径和任务 ID
  隔离，每任务 500 行、全库 50000 行、文件页数限制约 64 MiB。记录不依赖窗口打开，
  日志失败不得改变任务结果；不接受任意正文、URL 或错误字符串，不逐 token 记录。
  日志读取经服务重新核验当前任务归属，时间明确为 UTC，保留清理与无历史记录须可见。
  翻译任务的「文本块明细」不止由持久游标推导：`BlockInspector` 的已处理/待处理来自
  游标（`completed` 就是 `next_ordinal`），「处理中」来自服务层投影
  `BackgroundJobProgress::processing` —— 整本翻译会同时开多块，单看游标说不出在跑哪几块；
  旧格式游标、写坏的 JSON、非运行态一律当作「没有在飞的块」，不得凭空把游标那一块显示成
  处理中。**这份窗口必须由服务层解析后交给界面**：运行中的翻译任务不下发原始游标
  （`BackgroundJobSnapshot::cursor_json` 只留给失败任务和 `visual_render`），界面回去读那份
  永远缺失的游标就只会读到 0（现场 2026-09-20：并发 16 的翻译任务「已处理」一直在涨、
  「处理中」恒为 0，手工刷新也一样）。失败块必须来自该任务最新一次执行的
  `ProtocolSkipped`、`ProviderSkipped` 与带 ordinal 的 `StepFailed`
  事件（回滚式失败只把游标退回第一个未翻译块，命名不了整批失败块；两条跳过事件分别对应「模型
  答得不对」与「请求根本没答上来」，用户据此判断该换模型还是该查端点）。`SourceEchoed` 不进
  这份列表：它只说明库里那一行不算译文、walk 会重发这一块，紧接着那次请求要么翻好它、要么按
  上面两条跳过事件进列表，把它算成失败会让刚翻好的块一直挂在「失败」上。失败块从游标给的状态
  里移出，四个计数必须恒等于块总数；筛选、跳到当前进度、复制都要按同一个视图计算。
  任务失败时明细页顶部必须给出持久化的失败原因与本次未翻译的块编号，`RunFailed` 也要带
  `error_kind`，否则头部失败行只剩位置。  `JobLogErrorKind::label()` 是闭合表的固定中文说明，
  只能用于呈现，不得改写成错误正文。
  文本块明细每行的“查看”按钮打开只读调试面板（`TranslationBlockDetail`）：它**不读**任何
  持久化的请求副本——请求 body、系统提示与用户消息从不入库（见 `ai/job_diagnostics.rs` 的闭合
  事件约定），面板按被钉住的修订**重新解析文本块并按需重建** `translation_request` ——
  **单块**请求：这一块单独发送（「每请求文本块数」为 1、批回退到单块、单块重发）时 worker 发出的
  就是这一份；端点把多块打包进一个请求时的批信封形状取决于运行时打包，面板不假装能显示它。
  因此 worker（`translate_block`）与检查器必须共用同一个 `translation_prompt()`，不得各写
  一份（批路径的系统提示是同一前缀加批附加段，见 `prompt.rs`）；生成参数（温度、Top P、最大
  输出、两种惩罚）也共用一条构造路径，取的是「模型设置」
  窗口里翻译模型那一组（默认温度 0、上限 4096），两边一起生效。温度在设置里留空 = 请求体
  里不带这个键，面板照实显示「未设置（服务端默认）」而不是 0。
  模型名取任务的持久游标（任务执行过就以它钉住的模型为准），只有从未执行的任务才回落到
  AI 设置里当前配置的翻译模型（「模型设置」窗口 → 翻译模型，可能为空）。读取路径与列表同为**只读**：不认领、
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
  AI 回复用与 `gpui-kit` 组件层相同的固定版 `markdown` 解析器生成展示投影，按消息
  缓存；链接、图片与原始 HTML 只显示文字，不能让模型正文自动加载资源或打开 URL。
  Markdown 渲染保留自然高度和外层会话滚动，复制与持久化使用原文；来源按钮继续走
  宿主引用校验。  每条消息的气泡下方是操作条：两种角色都提供复制与朗读（正在朗读这条时图标
  换成停止，再点一次停止），用户问题另提供「修改」与「删除」，AI 回答另提供「重新生成」
  （状态与错误提示没有操作条）。朗读接入共享宿主层（`src/ui/ai_sidebar/speech.rs` 实现
  `ReadAloudWindow`）：一条消息一个会话，块 id 以消息 id 为前缀，回答先经
  `markdown::assistant_read_aloud_paragraphs` 去掉 Markdown 记号（代码块、表格与原始 HTML
  跳过，链接与图片只读标签）；侧栏不做逐段高亮，按钮图标即状态。修改把问题原文放回输入框并聚焦，
  重新生成直接重问该回答对应的问题，两者都先把会话回卷到对应消息再用替换的一轮续写
  同一前缀，所以重新生成历史回答会一并替换它之后的对话；删除移除该问答对并保留其它
  轮次。三者都以消息的 `chat_messages.id` 为锚点（`ConversationState` 的
  `turn_deletion`/`begin_edit`/`begin_regenerate` 决定动作，恢复的会话立即带上，
  实时回答由 `finish_persisted_answer` 回填）。删除走控制器在存储侧真正删掉之后重新
  加载会话再发布消息，因此存储失败不会让界面上出现一条其实没删掉的记录；编辑与重新
  生成把锚点随请求带出，拿不到锚点时只改本地投影，绝不用猜测的位置动词库里的其它
  轮次。操作条的铅笔、圆形箭头与朗读/停止喇叭是项目自有的 `assets/icons/pencil.svg`、
  `refresh-cw.svg`、`volume-2.svg`、`square.svg`，删除用 `gpui-kit` 组件层的
  `IconName::Delete`；自有图标由
  `src/ui/assets.rs` 的 `AppAssets` 在 `gpui-kit` 图标之外补充，`main.rs` 必须
  以它注册资源。TextView 在 flex 消息列内必须获得扣除 padding 与边框后的明确像素
  宽度；仅用 `w_full()` 会使 Windows 上的短列表正文被裁掉，测试字体不一定复现。
  `src/ui/ai_sidebar/` 保存展示清洗、GPUI 选择、流式更新与消息操作回归；
  `src/ui/ai_controller/tests/delete_turn_test.rs` 用真实服务覆盖删除一轮问答的三条
  回载路径（成功重载、陈旧锚点、存储失败），断言存储结果与界面投影一致。
  图书库的视图状态（分组筛选、排序字段与方向、卡片 / 列表）和它们的纯函数都在
  `ui/library/state.rs`；排序只在 `visible_book_indices()` 的出口按当前选择应用一次，
  卡片与列表因此共用同一顺序、同一份右键菜单（`book_menu.rs`）和同一套行级操作，
  布局差异只在 `render_books.rs` 与 `render_list.rs`。排序与展示方式是窗口内的视图
  选择，不写入 settings；列表行与卡片一样双击打开，行的纵向顺序可以用
  `book-row-<id>` 选择器断言。
  编辑器导航按独立 `TocNode` 树显示，节点 ID 与正文单元 ID 分开保存，不能用目录
  序号定位线性正文，也不能将同一单元的多个目录项合并。`src/ui/editor/navigation.rs`
  提供树展平和按节点缩进/提升；正文保存仅在章名实际改变时更新同名目录，保留
  独立子目录标题。EPUB 导入的 fragment 当前尚未映射为块目标，编辑器子目录定位到
  所属内容单元，不可按目录标签或序号猜测块位置。