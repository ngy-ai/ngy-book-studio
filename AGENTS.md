# AGENTS.md

## 适用范围

本文件适用于仓库根目录及其所有子目录。若以后在子目录中增加更具体的
`AGENTS.md`，以距离目标文件最近的说明为准。

本文件只保留**全局约束**（项目概览、文档职责、工作边界）与**文档索引**；按主题拆分的
详细文档放在 `docs/agent/` 下，处理对应模块前先读索引里指出的那一份。新增内容按主题写进
对应文档，不要再堆回本文件；任何一份文档接近约 700 行时同样按主题继续拆。

## 项目概览

“墨页”是一个面向 Windows 10/11 的本地图书库、阅读器与编辑器。项目使用 Rust
2024、GPUI、`gpui-component`/WebView2 和 SQLite。导入层将 EPUB、PDF、Office 与
DRM-free Kindle 文件转换为项目自有的 `BookDocument`；第三方解析器 IR 不得进入
数据库、UI 或其它公共领域模型。

当前支持导入 EPUB、PDF、DOC/DOCX、PPTX、XLSX、MOBI/AZW/AZW3，统一模型编辑，
EPUB/PDF 导出和原件导出。不要把回写原 Office/Kindle 格式、DRM、音视频转写、S3、
同步或发布打包视为已支持能力。

Windows/MSVC 是当前验收平台。依赖虽然启用了部分 Unix 图形后端，也不要据此宣称
应用已完成跨平台验证。

`ROADMAP.md` 表达实现状态和后续边界。判断当前行为时，以源码、测试、GUI 实测和
`README.md` 的一致证据为准。

## 文档职责

- `README.md` 面向使用者，只保留产品简介、用户可见功能、运行方式、本地数据行为
  和当前能力边界。
- `ROADMAP.md` 记录已实现能力与明确未实现边界，不以计划勾选代替验收证据。
- `docs/agent/*.md` 面向代理：开发命令、测试矩阵、代码地图、实现细节和代理工作约束
  按主题拆在那里（见下方索引），不要再复制回 README，也不要另起一份平行文档。
- 代码注释里写「见 `AGENTS.md`」时，按内容指向 `docs/agent/` 下对应的那一份。
- 用户可见行为变化时同步更新 README 和 ROADMAP；仅实现方式或验证流程变化时更新
  `docs/agent/` 中对应的那一份。

## 文档索引

| 文档 | 内容 | 何时读 |
| --- | --- | --- |
| `docs/agent/code-map.md` | 模块拆分惯例；`main.rs`、`startup/`、数据目录设置窗口、`logging.rs`、`theme.rs`、`services/`、`runtime.rs` 与顶层测试目标 | 改启动流程、数据目录、日志、主题、服务组合时 |
| `docs/agent/code-map-documents.md` | `document/`、`formats/`、`markup/` 与编辑/导出、存储与媒体、`library/`、笔记表、`db/` | 改统一模型、导入器、编辑/导出、图书库、笔记与数据库时 |
| `docs/agent/code-map-ai.md` | `ai/` 全部：OpenAI-compatible 访问、检索、索引与整本翻译后台任务、只读 Agent、分段协议与诊断 | 改 AI 后端、检索索引、翻译任务或 Agent 时 |
| `docs/agent/code-map-preview-office.md` | `preview/`、Windows PDF 与 DjVu 渲染器、Office COM/增强预览、`reader.rs`、`epub_limits.rs` | 改派生页面渲染、Office 增强、EPUB 投影与资源授权时 |
| `docs/agent/code-map-ui.md` | `ui/` 各窗口、AI 设置五窗口、后台任务窗口、编辑器导航 | 改 GPUI 界面与窗口时 |
| `docs/agent/code-map-reader.md` | Reader 笔记宿主、双语对照、PDF 页面笔记与笔记浏览窗口 | 改阅读窗口的笔记、译文或前端交互时 |
| `docs/agent/code-map-web.md` | `web/editor`、`web/pdf` 源码与前端构建产物契约 | 改前端源码或重建产物时 |
| `docs/agent/learning.md` | 学习中心模块与 `courses/` Python 课程实验包 | 改学习窗口、学习档案或课程包时 |
| `docs/agent/environment-and-commands.md` | 运行环境、常用命令、nextest 与 DOM 门禁、真实语料门禁、数据目录与日志落点、AI/导入/阅读器诊断日志 | 要跑命令、排障或抓日志时 |
| `docs/agent/constraints.md` | 编码与架构约定、统一模型与格式约束、数据与对象一致性约束 | 动手写代码前 |
| `docs/agent/ai-safety.md` | 搜索、视觉与 AI 安全约束（检索权限、后台任务、引用与模型参数） | 改检索、视觉派生数据或 AI 权限/引用时 |
| `docs/agent/webview-and-window-constraints.md` | 不可破坏的 WebView 与窗口约束、不可破坏的内容安全约束 | 改 WebView、窗口生命周期、编辑器与 IPC 时 |
| `docs/agent/gui-verification.md` | GUI 冒烟与完成标准 | 交付与验收前 |

## 工作边界

- 开始前运行 `git status --short`，保留用户已有文件和改动。不要用
  `git reset --hard`、`git checkout --` 等方式恢复不属于自己的内容。
- 不要手工修改 `target/`。不要把本机数据库、对象目录、测试导出文件、外部语料或
  WebView2 用户数据加入仓库。
- 自动测试和人工冒烟不得读写用户真实的 LocalAppData 图书库。测试优先使用运行时
  fixture、`tempfile` 或下文的唯一隔离数据目录。
- 保持 `Cargo.lock` 可复现。普通构建、测试和运行都使用 `--locked`；只有明确增删
  依赖时才更新锁文件，并检查更新范围。
- 不要顺手升级 GPUI、`gpui-component`、Wry/WebView2、`rbook`、`office_oxide`、
  `ebook-rs`、PDF.js 或 `sqlite-vec`。这些依赖已固定并涉及 Windows 句柄、解析安全、
  资产一致性或数据库 ABI；确需升级时按高风险变更重新走完整门禁。
- 许可证口径（2026-09-23 已由用户裁定，不要再改动除非他改口）：`LICENSE` 全文是
  「源码公开供阅读，禁止任何形式的封装/再分发」。`Cargo.toml` 已同步为
  `LicenseRef-NoRedistribution` + `license-file`。含义是：**作者本人**可以在 GitHub
  发布官方构建产物；**第三方**不得再封装或再分发。因此不要替它挑一个标准开源许可证，
  也不要宣称本项目是 Apache-2.0 / MIT 等。打包产物里必须带上 `LICENSE` 原文。
  官方 Release 为 Windows/macOS/Linux 三平台预编译包，其中 macOS/Linux 仅验证构建、
  未验证运行，Release 说明必须照实写明。

## 验证与完成标准

交付前按 `docs/agent/gui-verification.md` 执行，命令与门禁见
`docs/agent/environment-and-commands.md`。Rust 改动至少通过格式检查、全目标编译和全目标
测试；界面改动还要走完受影响路径的实际 GUI 验证。没有实际执行 GUI 路径时，必须明确写成
“未做人工 GUI 验证”，不能仅凭编译或单元测试宣称运行时问题已经解决。
