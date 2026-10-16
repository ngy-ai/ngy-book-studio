# GUI 冒烟与完成标准

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

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

> 默认 `production` 模式终端没有 tracing 输出，这条命令抓不到日志；要按 `RUST_LOG=error`
> 检查 HWND/WebView 错误，需先设 `$env:MOYE_APP_MODE = "development"`，或改去读
> `<NGY_DATA_DIR>\logs\` 下的日志文件（详见「数据目录与日志位置」）。

涉及 UI、WebView、导航、编辑器、Office 或窗口生命周期时，除自动验证外还要实际
走完受影响路径。按范围覆盖：启动；导入 EPUB/PDF/Office/DRM-free Kindle；结构化和
PDF 预览；新建图书；HTML、目录、封面和媒体编辑；保存；三种搜索模式；
AI 侧栏与引用；原件/EPUB/PDF 导出并重新打开；多 Reader/Editor 窗口；WebView 构建中
与构建后关闭。

Office 相关改动要在安装和未安装 Office 的环境分别验证，并覆盖超时/取消/回退。
AI 相关改动优先以 mock OpenAI-compatible 服务验证 SSE、工具、范围和取消；真实
Ollama 冒烟不得自动启动服务或下载模型。媒体改动要覆盖 seek/Range 和缺失资产。

翻译批请求（端点的「每请求文本块数」）的人工验证步骤：准备一台**远程**（非回环）端点、一个可用
的翻译模型和一本多章的书，在「Endpoint」窗口把「每请求文本块数」填 4 保存 —— 留空或 0 都读作
「自动」（输入框保持为空，占位提示写着本机一次一块、远程最多 8 块），填 1 表示关闭打包，2–64
是固定上限。跑一次整本翻译：任务窗口「文本块明细」里的“处理中”块数不得超过「任务并发 × 4」，
进度、译文与缓存仍按块推进（批里每一块都落到自己的块号，不存在“整批才算一块”）；一次运行
收尾后读 `<NGY_DATA_DIR>\logs\` 里 `ngy_ai` 的 `translation_run_stats` 一条（在日志文件里的呈现
是 `stage: "translation_run_stats"`，按等号搜会搜不到；**任务还在跑时没有这条**，跑到书末、
暂停、取消或失败后才打；进程重启或任务被替换接上的那次运行也不会有，先搜 info 级的
`translation_run_finish` 定位收尾；**恢复任务不是收尾** —— 恢复会开一次新运行
（`translation_run_id` 变新、`translation_run_start` 重新出现），要看读数必须再暂停一次或等它
跑到书末）。恢复后的第一条 `translation_batch_start` 的 `first_ordinal` 应等于上次暂停时 stats 里的
`next_ordinal`：在飞的块不提交、重做。`batched_requests` 应大于 0、`batched_blocks` 至少是它的两倍（一发
批请求至少带走 2 块），省下的请求数就是 `batched_blocks - batched_requests`。想不等收尾就对账：
数 `translation_batch_start`（每发批请求一条）与 `translation_batch_accepted`（每发被接受一条），
用 `first_ordinal` 的推进量除以条数就是平均每发带走几块。把该端点改回 1（或把书换到本机端点用「自动」）再跑一遍，
`batched_requests` 必须回到 0。若某块形状不合格，任务日志与「文本块明细」里应只出现那一块的
固定分类（片段数量不符一类的协议分类）、同批其它块照常落库；信封错误或整批被输出上限截断只让
这一批拆半重发，不得出现整批块一起进「文本块明细」的失败列表。

关闭图书库时验证继续运行保留全部窗口、确认退出关闭全部窗口，以及编辑器取消/保存
失败中止退出、再次关闭可重试；确认保存退出后重新打开应保留保存内容，且
`RUST_LOG=error` 不应出现 HWND/WebView 错误。在非 100% DPI 下自动化时区分截图逻辑
坐标与输入物理坐标，优先使用控件命中或 DPI 感知坐标。

没有实际执行 GUI 路径时，应明确写成“未做人工 GUI 验证”，不能仅凭编译或单元测试
宣称运行时问题已经解决。交付前报告实际命令、结果和未验证边界；Rust 改动至少通过
格式检查、全目标编译和全目标测试，运行时界面改动还需完成与风险相称的 GUI 验证。