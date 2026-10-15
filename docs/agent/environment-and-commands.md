# 环境与常用命令

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

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

## 数据目录与日志位置

数据目录在首次启动时由用户确认，不再使用 `ProjectDirs` 的默认位置；选择记录在
`%APPDATA%\ngy\ngy_book_studio\config\bootstrap.json`（实现见 `src/startup/`）。
设置界面是应用自己的窗口（`src/ui/data_dir_setup.rs`）：输入框里预填推荐路径或上次的
选择，可以直接改、可以点「浏览…」调系统目录选择器，点「确认并启动」（或回车）后才校验
并记录；校验在后台执行器上做，失败原因显示在同一个窗口里。关闭设置窗口即放弃启动。
日志固定在 `<数据目录>/logs/`，按 UTC 日期分文件、保留最近 7 个文件。目录写入配置之前
必须通过“建目录 + 写删探针”校验；记下的目录之后不可用时会重新询问并说明原因，不会静默
回落默认目录。系统配置页显示两个目录位置，并提供更改数据目录与打开目录的入口。

日志落点由运行模式决定：`production`（默认）只写 `<数据目录>/logs/` 下的 JSON 文件，
终端没有任何 tracing 输出；`development` 只写终端（pretty，带源码文件和行号）；
`test` 两者都写。因此下文几段从终端抓日志的排障处方，都必须先把 `MOYE_APP_MODE` 设成
`development`（或 `test`）才有内容可抓；保持默认 `production` 时应改去读日志文件。
模式由 `MOYE_APP_MODE`、`.env` 或第一个命令行参数设置（优先级见 `main.rs::install_logging`）。

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

## 可选测试执行器（cargo-nextest）

`cargo-nextest` 已在本机可用（0.9.145），配置在仓库根 `.config/nextest.toml`，但它
不是门禁的替代品：`cargo fmt` / `cargo check` / `cargo clippy` 仍由 cargo 执行，上面的
`cargo test` 命令仍是最终验收基线。切换某条命令前先做一次全量对账并把项数差异写进
`ROADMAP.md`，不要凭「nextest 默认更全」直接替换。

命令映射 —— `--` 之后的 libtest 参数在 nextest 里不存在，必须换成 nextest 自己的选项：

| `cargo test` | `cargo nextest run` |
| --- | --- |
| `--all-targets` | 不需要，nextest 默认跑全部测试目标 |
| `--no-fail-fast` | 不需要，nextest 默认不 fail-fast |
| `... -- --ignored` | `... --run-ignored ignored-only` |
| `... -- --nocapture` | `... --no-capture` |
| `... -- --test-threads=1` | `... --test-threads=1`（语义已变，见下） |
| `... <名称>` | `... -E 'test(<名称>)'` |

例：`cargo nextest run --lib --locked -E 'test(learning::)' --run-ignored
ignored-only --test-threads=1` 对应 `cargo test --lib --locked learning:: -- --ignored
--test-threads=1`。

差异与注意：

- nextest 不跑 doctests，需要时另跑 `cargo test --doc`；本仓库当前没有 doctest。
- 每个测试跑在独立进程里，测试之间不再共享进程状态，泄漏的线程与全局状态不会跨测试
  传播；代价是大二进制（gpui + test-support）被反复加载，`--test-threads` 现在控制
  并发**进程**数，CPU 与内存峰值比单进程 `cargo test` 更高。
- 因此 `--test-threads=1` 从「单进程单线程」变成「同时只跑一个测试进程」。learning
  的真实窗口 / 键盘注入专项切换前必须实测通过，不能假设等价；`.config/nextest.toml`
  用 `learning-desktop` test-group 把 `supervisor_control_flow` 与 `desktop_*` 限制为
  独占运行，用 `background-gated` 把 `services::tests::` 限到 2 并发。
- 2026-09-17 全量对账：975 项运行 / 8 项 skip、约 51 秒。默认并发下 `services::tests::`
  有 4 项因紧截止预算耗尽假失败（同批在低并发与 `cargo test --lib services::tests::`
  下全通过、单项连跑 3 次全通过），按上面的 test-group 限流后复跑消失；当时仍失败的
  7 项都在集成测试 `translation_flow`，且 `cargo test --test translation_flow` 同样
  失败，属工作区未提交改动的既有状态，与本迁移无关。
- 那批紧截止夹具（`ai::indexing`、`translation_flow`）在更高并发下更容易偶发失败，
  这是测试自身的预算问题而非产品回归；配置里 `retries = 0` 是刻意的，先修预算，不要
  用重试掩盖。

## 可选 DOM 门禁（Node + 已安装 Playwright）

`src/ui/reader/position.test.cjs`、`src/ui/reader/translations.test.cjs`、
`src/ui/reader/annotations.test.cjs`、`src/ui/reader/read_aloud.test.cjs`、
`src/ui/pdf_reader/annotations.test.cjs`、`src/ui/pdf_reader/viewer.test.cjs`、
`src/ui/pdf_reader/read_aloud.test.cjs` 用
`node --test <file>` 运行。它们只使用本机已安装的 Playwright，不为测试安装依赖、
不触碰用户数据，`NGY_TEST_CHROMIUM` 可指定浏览器。改动 `src/ui/**/*.js` 的交互代码
或 `web/pdf` 产物时建议跑对应门禁，并在 `ROADMAP.md` 或本文件记下结果。

本机（2026-10-10 复核）可用组合：Node 22.22.2 用工作台自带的
`C:\Users\admin\.workbuddy\binaries\node\versions\22.22.2-6\node.exe`（工作台升级会换目录，
旧版本路径随即失效），Playwright 1.63.0 借自 `%TEMP%\moye-pw\node_modules`（临时目录，里面
另有 `*.cjs` 诊断脚本；被清理后用 `npm install playwright@1.63.0 --prefix %TEMP%\moye-pw`
重装，先设 `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1` 避免再下载浏览器），`NODE_PATH` 指向它。
**浏览器必须显式指定**：
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

2026-10-10 首次在本机跑齐六个门禁：63 项全部通过（`reader/translations` 26、
`reader/annotations` 20、`reader/read_aloud` 3、`pdf_reader/annotations` 4、
`pdf_reader/viewer` 5、`pdf_reader/read_aloud` 5）。首轮的两个失败都定位成用例缺陷
（滚动锚定补偿与缺等待），修法与证据见 `code-map-reader.md` 的 2026-10-10 记录，
产品代码没有改动。

2026-10-13 复核七个门禁：69 项全部通过（`reader/position` 6、`reader/translations` 26、
`reader/annotations` 20、`reader/read_aloud` 3、`pdf_reader/annotations` 4、
`pdf_reader/viewer` 5、`pdf_reader/read_aloud` 5）。新增的 `reader/position` 覆盖：折叠
范围仍带行盒、首可见行随滚动前移且引文长度与范围自洽、恢复把位置滚回视口、
过期锚点不动页面、位置没变时不重发、译文层上的采样映射回原文块。同批 Rust 门禁：
`cargo test --lib` 706 项（701 通过 / 5 忽略，忽略项都是需要课程环境的 `learning` 用例）、
`cargo test --bin ngy-book-studio` 351 项（350 通过 / 1 忽略，忽略项是需要外部样本的
`xhtml_test`）、`cargo test --test epub_flow_test` 6 项通过。同日端点限速（RPM）改动新增
16 项 lib 与 2 项 bin 测试后复跑全目标：`cargo test --lib` 722 项（717 通过 / 5 忽略）、
`cargo test --bin ngy-book-studio` 353 项（352 通过 / 1 忽略）、
`cargo test --test openai_compatible_flow` 23 项、`cargo test --test multi_endpoint_flow_test`
1 项、`cargo test --test translation_flow` 20 项（19 通过 / 1 忽略）。

2026-10-13 修「译文」模式原文未隐藏（朗读小喇叭被当成书里的媒体），并把朗读的小喇叭、高亮与
视口起读改按「读者看得见的那份文字」落位，之后重跑 `reader` 四个门禁：59 项全部通过
（`reader/translations` 26、`reader/annotations` 20、`reader/position` 6、`reader/read_aloud` 7，
朗读门禁从 3 项增到 7 项）。`reader/translations` 现在与生产同框加载朗读脚本、并断言页面确实
带着这枚 chrome，所以「两个门禁各看一半」这条回归以后会被它拦住。反向验证过两次：把
`hasBookMedia()` 临时改回 `element.querySelector(MEDIA)`，「译文」两项立刻失败；把
`visibleHost()` 临时改回「永远返回块本身」并去掉高亮的内联覆盖，朗读门禁 7 项里 6 项立刻失败。
本次只改 `src/ui/**/*.js` 与文档，没有跑 Rust 门禁；未做真实 Windows GUI 手工复验（见
`ROADMAP.md` 同日两条记录）。

2026-10-15 翻译任务对账改「一步一个单位」与让路、失败诊断带端点来源、冷却与「已领取但无
进展」进任务日志并加静默看门狗（实现与回归见 `ROADMAP.md` 同日记录）之后复跑 Rust 门禁：
`cargo fmt --all --check` 通过，`cargo clippy --all-targets --locked` 110 条既有告警
（与改动前同数、无新增），`cargo test --locked --all-targets --no-fail-fast` 全通过 ——
lib 732 项（727 通过 / 5 忽略）、产品二进制 357 项（356 通过 / 1 忽略）、
`openai_compatible_flow` 24、`translation_flow` 20 项（19 通过 / 1 忽略）、
`epub_flow_test` 6、`multi_format_flow_test` 4、`multi_endpoint_flow_test` 1、
`proxy_flow_test` 1、`format_corpus_gate_test` 1 忽略。2026-10-13 观察到的
`services::tests::` 并发预算假失败本次没有复现（同一批用例在默认并发下通过）。
未做人工 GUI 验证（见 `ROADMAP.md`）。

2026-10-15 阅读端逐块「重新翻译」改「已有译文也强制重发」、运行中的整本翻译先落盘再规划、模型
原样抄回原文不再算译文（实现与回归见 `ROADMAP.md` 同日记录）之后复跑 Rust 门禁：
`cargo fmt --all --check` 通过，`cargo clippy --all-targets --locked` 与改动前同数、本次触及的
文件无新增告警，`cargo test --locked --all-targets --no-fail-fast` 全通过 —— lib 737 项
（732 通过 / 5 忽略，比上一批多 5 项：`translation_retry_test` 2、
`ai::translation::tests::segment_test` 2、`services::tests::translation_test` 1）、
产品二进制 357 项（356 通过 / 1 忽略）、`openai_compatible_flow` 24、
`translation_flow` 20 项（19 通过 / 1 忽略）、`epub_flow_test` 6、`multi_format_flow_test` 4、
`multi_endpoint_flow_test` 1、`proxy_flow_test` 1、`format_corpus_gate_test` 1 忽略。
首轮全目标运行里有一项失败：`services::tests::visual::jobs_test::
startup_requeues_legacy_renderer_and_reclaims_its_visual_object` 等待视觉任务进入 `Succeeded`
超时（当时状态仍为 `Running`），单独复跑与随后的两次全量复跑都通过，属上面 2026-09-17 记的
「`services::tests::` 紧截止预算在默认并发下假失败」那一类。人工 GUI 验证（2026-10-15，用户
实测）：整本翻译运行中点「重新翻译这一块」，先暂停再重排，不再报「这本书没有可重试的翻译任务」。

## AI 问答诊断日志

AI 日志统一使用 `ngy_ai` target。未设置 `RUST_LOG` 时，默认
`warn,ngy_ai=info`，记录问答开始、完成、取消及失败；详细排障使用
`warn,ngy_ai=debug`。默认 Production 模式只写数据目录下的
`logs/ngy-book-studio.<YYYY-MM-DD>`（见 `src/logging.rs`）；Development 模式只写终端，
输出带源码文件和行号。数据目录确定之前的启动阶段没有日志，需要整段启动过程时，
在 PowerShell 中运行：

```powershell
$env:RUST_LOG = "warn,ngy_ai=debug"
$aiLog = Join-Path ([System.IO.Path]::GetTempPath()) ("ngy-ai-" + [guid]::NewGuid() + ".log")
cargo run --locked --bin ngy-book-studio 2>&1 | Tee-Object -FilePath $aiLog
```

> `Tee-Object` 只截获终端输出；默认 `production` 模式终端没有 tracing 输出，需先
> `$env:MOYE_APP_MODE = "development"`（或 `test`），详见「数据目录与日志位置」。

开发验证还须按下文 GUI 冒烟要求设置唯一 `NGY_DATA_DIR`。环境变量只作用于从该
终端新启动的进程；复现结束后恢复原来的 `RUST_LOG`，或原来未设置时用
`Remove-Item Env:RUST_LOG` 移除。不要用全局 `trace` 抓取 HTTP 请求正文。

- `trace_id` 是进程内跨窗口唯一的问答编号，从请求注册、冻结选区、作用域与历史、
  模型/工具调用到保存和界面完成共用；重启后重新计数。`request_id` 是窗口内代次。
- `http_id` 标识一次 HTTP 调用；`round` 是工具轮次，`attempt` 是该轮启动流的尝试。
  `stage` 与 `error_kind` 区分 HTTP 拒绝、流读取、JSON/工具校验、来源校验和保存失败。
  `AI request token invalidated` 也会在正常释放请求时出现，不代表用户取消。
- HTTP 记录主机、端口、模型、请求字节数、超时、状态码和受控错误类型；4xx/5xx 的
  `http_rejected` 额外记录 `provider_param`：拒绝信封点名的**自有请求字段**。它只可能是
  固定列表里的名字（`reasoning_effort`、`stream_options`、`max_tokens` 这类蛇形传输字段
  名），服务端的文本一个字节都不进日志；认不出来时为 `-`。流结束记录
  事件数、首事件等待、耗时、`finish_reason` 与 provider 实际返回的 token usage。
  `length` 可作为输出达到限制的排查线索，不能单独证明 JSON 截断的根因。
- 工具仅记录允许的名称、参数字节数、JSON 类型/字段数量、已知数组长度和解析错误
  分类/行/列。未知工具名和 provider 标识归一化；不记录提问、书名、正文、选区、
  回答、完整参数、引用 ID、密钥、HTTP headers 或端点路径/查询串。
- 不逐 token 打日志。新增日志必须使用同一 target、继承异步 span，并经
  `src/ai/ai_diagnostics.rs` 的分类/摘要函数处理不可信值；不能直接格式化任意错误链。
  HTTP/SSE 故障与日志脱敏回归位于 `tests/openai_compatible_flow.rs`。

## 导入诊断日志

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

> 同样地，默认 `production` 模式终端没有 tracing 输出，需先
> `$env:MOYE_APP_MODE = "development"`（或 `test`），详见「数据目录与日志位置」。

导入路径上的日志只记录字节数、记录序号、格式、置信度、媒体类型、稳定 ID 与受控错误
分类；不记录正文、译文、选区、密钥或完整文件内容。用户可见的失败仍以界面提示为准，
日志只用于定位阶段，不能替代界面上的错误信息。

不需要 GUI 复现一次导入时，`examples/probe_import.rs` 会走同一格式层与
`LibraryStore` 路径，把库落到临时目录并在其后重新打开，用于区分“解析失败”与
“持久化/回读失败”。它不写真实数据目录，但也不覆盖界面状态机与文件对话框。

## 阅读器诊断日志

阅读器资源供给统一使用 `ngy_reader` target，未设置 `RUST_LOG` 时同样默认开启 debug：
每章打印请求路径、媒体类型、正文字节数、被改写的命名实体名与数量；每份二进制资源打印
状态码、字节数与 Range；`export.rs` 生成阅读器投影时每章打印一次实体改写汇总。
“章节打开后是渲染错误页而不是正文”这类反馈先看这里——它只报告实体名和计数，不含正文。

因为 WebView 只给出 “Entity 'xxx' not defined” 和行号，脱离 GUI 定位这类问题用
`examples/probe_reader.rs`：它导入到临时库、走 `LibraryStore::reader_epub_bytes`
生成投影，再用 `reader::load_resource` 取出 WebView 真正会收到的字节，按行/列报告
XML 解析不了的命名实体。

## 书库操作诊断日志

书库写入（改名、改作者、分组、移动、删除、导入落库、编辑保存、阅读进度）统一使用
`ngy_library` target。未设置 `RUST_LOG` 时默认开启 debug：
`spawn_library` / `spawn_library_projected` 在**每个等待前后各记一行**——已排队（ticket）、
已获准进入（等待写入许可的毫秒数）、已取得共享书库锁（等待毫秒数）、已提交/已失败
（执行毫秒数）；排队 FIFO 的派发器在一张许可被占用超过 5 秒时告警并打出 ticket。界面层
另记「开始/完成/失败」与请求号、耗时，元数据提交（`update_catalog_metadata`）和文档版本
提交（`publish_document_with_assets`）各自记一行 info，重发表文档前还会记下
`units` / `toc_entries` / `search_chunks` / `assets` 数量。

因此“界面一直显示处理中、控制台什么都没打”这类反馈有了第一手依据：最后一条 `ngy_library`
日志就是卡住的那一步（停在“已排队”＝卡在写入许可；停在“开始执行”＝卡在该操作内部，日志里的
分块数量说明它是不是在重发表整本文档）。阅读器与导入的对应 target 见上文。

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