# 学习中心与 Python 课程实验包

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 学习中心模块

- `src/learning/`、`src/learning_records/`、`src/learning_catalog.rs`、
  `src/ui/learning/`：学习中心异步服务、逐章目录、
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
  滚动容器内，选区几何与文字一起移动。不得恢复 `selectable(true).scrollable(true)`
  组合：内部虚拟列表滚动会让选区与文字脱节。GPUI `test-support` 仅用于开发测试，
  事件与剪贴板回归不能替代真实 Windows 鼠标、字体和系统剪贴板验收。

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