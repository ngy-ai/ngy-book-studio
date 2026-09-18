# 代码地图（启动、服务与基础模块）

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 模块拆分惯例

各模块按职责拆成目录：`mod.rs`（或 `startup/mod.rs` 这类入口文件）只放模块文档、
共享导入、模块声明和对外的 `pub use`，实现分散在同目录的子模块里；子模块用
`use super::*;` 取得父模块声明的项，只在子模块之间共享的类型与函数按 `pub(crate)`
收口（测试夹具用 `pub(super)`）。新增代码放进对应的子模块，不要再把大文件堆回来。

## 模块

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
- `src/startup/`：数据目录的解析与引导配置。数据目录不再固定用 `ProjectDirs` 的
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
- `src/logging.rs`：日志目录与默认过滤串（`logs/` 子目录名、文件前缀与默认 `RUST_LOG`
  串）。全局 subscriber 由 `main.rs::install_logging` 调用 `ngy_utils_tracing::init` 安装，
  安装点必须晚于数据目录确定（日志目录由它推出，此前的启动阶段没有日志）。`ngy_utils_tracing`
  默认 Production 模式：文件层落到 `<数据目录>/logs/ngy-book-studio.<YYYY-MM-DD>`，JSON
  格式、按 UTC 日期滚动；目录创建与过期清理（保留最近 7 个日滚动文件，含当天）都由该库在
  `init` 时完成，并在后台定期执行，本项目不再自行实现。Development 只写控制台（pretty
  格式），Test 为控制台 + 文件。模式由 `MOYE_APP_MODE` 环境变量决定（首个命令行参数可覆盖），
  未设置时默认 Production。该库先加载 `.env` 再读取模式，所以 `.env` 里的
  `MOYE_APP_MODE` 也生效，并且因用 `dotenv_override`，其值会覆盖进程环境变量；命令行
  参数的覆盖优先级最高。
  该库（0.6.0 起）读取的环境变量一律带 `logging::ENV_PREFIX`（`MOYE_`）前缀，只有
  `RUST_LOG` 保持无前缀；`init` 还装一层 `log` 兼容桥，依赖里走 `log` 的记录此后也会
  进日志。时间戳偏移必须显式传 `logging::LOG_TIME_OFFSET`（`UTC`）—— 库的默认值不是
  UTC，不传就会与按 UTC 日期滚动的文件名不一致。
  日志目录不是独立选项：它固定是 `<数据目录>/logs/`。`main.rs` 持有 init 返回的
  `InitResult`（含文件 guard 与后台清理句柄）并在进程退出前 drop，以免丢掉非阻塞写入的
  尾日志、或让清理线程悬空；init 失败退回 `console_tracing`，两条路都装不上只报 stderr，
  不 panic（GPUI 回调不可 unwind），也不拦住启动。
- `src/theme.rs`：浅色 / 深色两套皮肤的唯一定义。`Appearance` 是**应用级**偏好
  （存 `appearance.preferences.v1`，由 `ProviderSettings.appearance` 合并进快照），
  `Palette` 给每个语义色一个取值，`set_appearance` 一次做完三件事：记住皮肤、把同一份
  颜色写进 `gpui-component` 的 `ThemeColor`（不写的话按钮、输入框、滚动条会留在旧皮肤
  上，而且必须在 `Theme::change` **之后**写，否则被内置主题覆盖）、刷新全部窗口。
  界面代码一律 `palette(cx).xxx` 取色，不要再写 `rgb(0x...)`：写死的颜色在深色皮肤里就是
  「深色叠深色」。取色点拿不到 `cx`（自由函数、`'static` 闭包）时先
  `let theme = palette(cx);` 取值再传：闭包里直接写 `palette(cx)` 会把 `&App` 借用带出
  方法体，`move` 闭包还会把 `cx` 移动走。半透明色用 `Palette::tint(基色, alpha)`，
  不要写 `rgba(0x...)`；文本选中底色是 `Palette::selection()`（半透明强调色），不要退回
  不透明的 `accent_soft` —— 它在深色皮肤里和 `surface`（AI 回答卡片、输入框）只差一档
  明度，选中范围看不出来。会承载文字的卡片一律用皮肤色（回答卡片 `surface` + `ink`，
  用户气泡 `accent` + `paper`），写死的 `white()` 配 `ink` 在深色皮肤里是浅底浅字。
  WebView 拿不到 Rust 常量，三个面（阅读器章节、富文本编辑器、
  PDF.js viewer）靠 `Palette::css_variables` / `css_variable_setters` 下发的 `--ngy-*`
  自定义属性取色；编辑器还要额外注入一层覆盖规则（`editor_appearance_script`），因为它
  的颜色写死在随包发布的 `assets/editor/prosemirror.js` 里。各 WebView 窗口用
  `observe_global::<AppTheme>` 跟随切换，不靠中心化的窗口注册表；窗口自己的
  `with_background_color` 要在创建时就是当前皮肤的颜色，否则每次换页会闪一帧旧底色。
- `src/services/`、`src/runtime.rs`：进程级服务组合与独立 Tokio runtime；统一持有
  图书库、对象存储、格式注册表、搜索、AI、Office 和后台任务。GPUI 回调跑在自己的
  executor 上，不是 Tokio 上下文：UI 可达的服务方法必须把数据库、对象存储、`tokio::fs`
  和解析工作放到应用 I/O runtime（`IoRuntime::spawn`）上再 await，`runtime.rs` 的
  `block_on` 只用于启动、测试与后台线程。索引协调器内部 `run_db`/`run_db_mut` 解析
  环境句柄，只允许它自己派发的工作使用；公共路径用 `run_db_on` 显式传入句柄，否则会
  在 GPUI future 里 panic，而 GPUI 回调不可 unwind，会直接终止整个进程。
  `src/services/` 测试里的 `block_on_without_tokio` 用非 Tokio 执行器驱动服务方法，
  用于固定这条约定（后台任务窗口的文本块明细读取曾因此崩溃）。
  `load_published_visual_pages(book_id, kind)` 是 Office 增强预览与 DjVu 共用的页面读取
  入口：它在 `library_mutations` 与 `blob_publication` 门闩内读 `visual_pages`、逐页校验
  长度/BLAKE3/`BlobKey`/图片格式后才返回字节，`load_office_enhanced_pages` 只是它的
  委托包装；新增页面来源时必须走同一入口，不要复制门闩或摘要校验。
  `ensure_visual_render_job` 只用于本地可安全自动运行的渲染（DjVu），Office COM 增强
  必须继续走显式信任对话框与 `set_office_enhancement_enabled`。
  `AppServices::configure_providers` 是 AI 设置保存的唯一入口，它串行地拿两把门闩：
  `ai_configuration`（整个保存期间持有，保证一次设置切换不被交叉）与索引协调器的
  `transitions`。**等门闩必须有上限**（`AI_SETTINGS_GATE_TIMEOUT`，30 秒）：门闩持有者
  可能正被后台任务堵住（现场 2026-09-17：设置窗口停在「正在安全保存 Provider 设置…」，
  而 `settings` 表根本没被改写），无限等待会把之后的每一次保存也拖成永久挂起；超时发生在
  进入写入之前，本次没有提交任何东西，所以报「另一个设置保存仍在进行中」并让用户重试。
  门闩**里面**的重配置与回滚不能包超时——「要么提交、要么回滚」的保证要求那次工作跑完。
  界面侧另有一条等待上限（`src/ui/ai_settings/mod.rs` 的 `SAVE_WAIT_LIMIT`，60 秒）：超时只是
  放弃等待，写入仍留在运行中的任务里，所以提示必须写成「设置可能已经保存，也可能仍被阻塞」，
  不能写成失败；用户主动关窗不因为保存中而被拦（`on_window_close` 只在
  `application_is_exiting` 且 `Saving` 时等结果），否则一次被堵住的保存会变成关不掉的
  死窗口，用户除了杀进程没有别的出路。

## 顶层测试目标

- `tests/epub_flow.rs`：运行时生成 EPUB 2/3 fixture 的原有跨模块流程。
- `tests/multi_format_flow.rs`：统一模型、格式、对象存储、编辑、搜索和导出的多格式流程。
- `tests/openai_compatible_flow.rs`：mock OpenAI-compatible models/embeddings/SSE 流程。
- `tests/format_corpus_gate.rs`：仓库外真实格式语料的显式 ignored 门禁。
- `src/bin/gpui_hello.rs`、`src/bin/hello_world.rs`：示例二进制，不是产品入口。