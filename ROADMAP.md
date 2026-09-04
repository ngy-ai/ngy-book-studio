# ROADMAP

本文件记录当前源码中的能力和仍未完成的验收/边界。勾选表示已有实现与自动化覆盖，
不等同于已在所有真实文档和外部软件环境中完成 GUI 验收。

## 已实现

- [x] 统一文档模型
  - [x] 稳定 ID、revision、`BookDocument`、线性 `ContentUnit`
  - [x] 独立层级 `TocNode` 与可复用 `DocumentLocator`
  - [x] 段落、标题、列表、引用、代码、表格、图片、音频、视频和受限 RawHtml 块
  - [x] Markdown/HTML 解析、清洗、AST 与规范化序列化
- [x] 本地持久化
  - [x] SQLite 当前结构及按表拆分的 CRUD/查询映射
  - [x] 开发期不迁移：结构或数据关系不一致时重建数据库与受管对象目录
  - [x] 基于 `object_store` LocalFileSystem 的 BLAKE3 内容寻址 `BlobStore`
  - [x] 原件、封面、媒体与派生页面外置；先写对象再事务发布引用
  - [x] 删除先解除引用、后台回收对象及启动孤儿 GC
  - [x] 媒体归属校验、MIME、单 Range 与 `206` 响应
  - [x] 单个应用进程内由共享 `AppServices` 为多窗口 mutation 排序并发布单调投影
- [x] 导入、阅读与预览
  - [x] EPUB 2/3
  - [x] PDF（本地固定 PDF.js，禁用 JavaScript/XFA/联网；导入与逐页视觉共享
    256 MiB、4,096 页上限）
  - [x] DOC/DOCX、PPTX、XLSX（`office_oxide` 转统一模型）
  - [x] DRM-free MOBI/AZW/AZW3（`ebook-rs`，加密内容明确拒绝）
  - [x] 所有导入格式保留字节一致的原文件
  - [x] Office/Kindle 结构化预览与 PDF 固定版式查看
  - [x] 用户按书显式启用的 Office COM 增强预览：Word/Excel 经临时 PDF 光栅化且页面
    不猜测内容单元归属，PowerPoint 逐页图片保留精确幻灯片映射；最终页面按
    renderer/revision/profile 持久化到对象存储
  - [x] Office 原件只读、禁宏/外链更新、不主动调用宏/OLE、可取消，失败或未安装时
    回退结构化预览（仅建议对可信文件启用，不把 Office 解析视为安全沙箱）
  - [x] EPUB/PDF 阅读器和编辑器使用可拖动的三栏布局，左右栏分别限制在可用范围内，
    窗口缩小时保留中央正文空间且不破坏 AI 侧栏的自动收起行为
- [x] 新建与编辑
  - [x] 新建图书先要求用户输入非空名称，确认后创建默认包含 Markdown 章节的图书
  - [x] 编辑元数据、封面、内容单元类型及 Markdown/HTML 正文
  - [x] 新增、删除、重排内容单元，目录嵌套、缩进与提升
  - [x] 本地固定 ProseMirror 富文本编辑器及快照屏障
  - [x] 图片、音频、视频块的插入、替换、删除与本地播放
  - [x] 媒体标题/人工说明编辑及全文索引
  - [x] 编辑器只从统一 AST 生成安全预览，持久媒体按授权 ID 和 Range 异步读取
  - [x] 受信编辑器 shell 与不可信规范化正文使用不同 origin，IPC 只接受完整页面身份
- [x] 导出
  - [x] 从统一模型稳定导出 EPUB
  - [x] 从统一模型稳定导出 PDF
  - [x] 导入图书可导出字节一致的原件
  - [x] 统一模型可规范化导出 EPUB/PDF，不承诺复刻原 Office/PDF/Kindle 复杂版式
  - [x] 同目录临时文件同步后原子替换目标
- [x] 搜索与派生索引
  - [x] 稳定 chunk、FTS5 关键词搜索和图书范围硬过滤
  - [x] 静态集成 `sqlite-vec` 的精确 KNN
  - [x] 关键词、语义与 RRF 混合模式；向量不可用时回退 FTS
  - [x] 持久化 embedding、视觉渲染和 vision 队列及 revision 失效处理
  - [x] 后台任务暂停、恢复、重试、取消及进程重启恢复；视觉渲染逐页持久化断点，
    恢复时跳过已提交页面，完整页面集与成功状态原子发布
  - [x] 每个图书的当前来源只保留一个有效视觉理解任务；AI 配置未变化时不重复投递，
    视觉模型变化时替换旧任务；遗留重复/来源不明任务会清理旧派生结果后重建，且 AI
    配置与全部当前来源的索引契约原子切换、失败整体回滚
  - [x] embedding 重配置按规范化端点与 embedding 模型判定执行身份；聊天/视觉模型、
    超时和 API key 单独变化不重置已完成任务，embedding 模型或端点变化只首次投递；
    游标代次保护会清理旧向量并拒绝旧 provider 的迟到写入，同时保留视觉理解完成后的
    独立增量 embedding 任务
  - [x] 逻辑页面的实际 PNG 光栅化，以及带归一化页面区域的视觉说明/OCR chunk；
    Word/Excel 整本 PDF 无法证明页到单元映射时仅保留增强预览页，不生成视觉索引
- [x] OpenAI-compatible AI Agent
  - [x] 默认本地 Ollama `http://127.0.0.1:11434/v1`
  - [x] 低资源开发默认模型：聊天/视觉 `qwen3.5:0.8b`，embedding
    `qwen3-embedding:0.6b`，仍可在设置中分别修改
  - [x] models、流式 chat completions/tools/vision 与 embeddings；正文按 SSE 增量临时
    展示，tool call、取消、流错误或来源校验失败时撤回，校验成功后才提交最终回答
  - [x] 聊天、embedding、vision 模型分别配置，可设置 1–600 秒请求总超时；远程/非
    HTTPS 显式确认且绑定准确端点
  - [x] AI 设置窗口为应用级单例，重复打开时激活并恢复现有窗口，关闭后可重新创建
  - [x] API key 存 Windows Credential Manager
  - [x] 只读 `search_books`、`read_passages`、`get_outline` 工具和宿主范围硬过滤
  - [x] 图书库、Reader、PDF Reader、Editor 可折叠 AI 侧栏
  - [x] 每个窗口上下文可新建、列出和切换多个独立持久会话；空会话首次发送时才创建，
    切换时恢复对应消息和来源，流式请求期间阻止会话竞态
  - [x] 会话经逐项确认后可删除，其问题、回答和引用同步永久移除；删除当前或最后一个
    会话后回到空白新会话，删除非当前会话保留当前内容，跨窗口正在使用的会话拒绝删除
  - [x] 用户与 AI 历史消息支持文字选择和逐条原文复制；消息轨道占满侧栏可用宽度，
    长消息使用稳定最大宽度
  - [x] Office 增强页面窗口复用当前书 AI 范围；PPTX 逐张幻灯片保留精确 locator，
    Word/Excel 整本 PDF 页（即使页数与单元数相同）仅标记派生页码、仅供预览且不提供引用
  - [x] Library 当前分组后代范围；Reader/Editor 当前书必选并可追加其它书
  - [x] Reader/PDF Reader/Editor 的非空当前高亮默认加入本轮且可在右栏取消；清空高亮
    不会退化为整章引用，额外章节/页面仍显式多选，无选区时不向模型发送内部空快照数组
  - [x] “可选引用”列表最多显示三行，更多候选保留在列表内滚动访问，不挤占聊天记录区域
  - [x] 高亮、编辑器未保存选区及多章节使用 revision 精确快照；请求在异步冻结前即可
    取消，恢复时无法由当前 AST 精确证明的未保存选区标记为失效
  - [x] 持久会话、流式取消、受 scope/locator/document revision/unit revision
    校验的来源标记及可点击引用；Reader/Editor 重新校验 AST 并只定位唯一文字范围，
    伪造、越权、旧版本、缺失或歧义来源明确报错且不降级跳转
  - [x] 所选图书没有可验证来源时允许模型基于通用能力回答，并在答案开头明确标为
    “非知识库回答”

## 验收状态（2026-09-03）

以下勾选只代表对应门禁已执行，不会把仍未完成的真实 GUI、外部软件或安全语料验收
折叠成“整体已完成”。

- [x] 在 Windows/MSVC 上以仓库外九格式语料运行完整 `format_corpus_gate`：EPUB、PDF、
  DOC、DOCX、PPTX、XLSX、MOBI、AZW、AZW3 均完成导入、规范化编辑、FTS 搜索、
  EPUB/PDF/原件导出、原件字节校验及重新打开
- [ ] 扩充真实复杂语料：中文字体、公式、复杂/合并表格、旋转/扫描 PDF、PPTX
  母版/图表、隐藏工作表区域，以及损坏、加密和压缩炸弹文件
- [ ] 在安装与未安装 Microsoft Office 的 Windows 环境完成真实 GUI 增强预览验收，
  覆盖持久化页面重开、禁用、超时、取消和结构化回退
- [ ] 用隔离数据目录走完多格式导入 → 查看 → 编辑 → 保存 → 三种搜索 → AI 多会话
  新建/切换/重启恢复、消息选择/复制与引用 → EPUB/PDF/原件导出 → 重新打开的 GUI 矩阵
- [ ] GUI 覆盖 Ollama 离线/模型缺失、媒体 seek，以及 WebView 构建中和构建后关闭窗口
- [ ] 在真实 Windows Credential Manager 完成 API key 保存、读取、删除及失败路径验收；
  当前已实现平台存储并有依赖注入测试，但不能用内存 mock 代替真机凭据验收

## 明确不在当前范围

- [ ] S3 对象存储实现、本地/远程同步和多设备冲突处理
- [ ] DRM Kindle 导入或 DRM 绕过
- [ ] 修改后写回 DOC/DOCX、PPTX、XLSX、MOBI/AZW/AZW3
- [ ] 原 Office/PDF/Kindle 复杂版式的无损规范化导出
- [ ] Office 宏/OLE 主动调用、PDF/书内脚本或外部网络资源
- [ ] 同一数据目录的多应用进程并发、跨进程 mutation 排序或单实例锁；当前仅支持一个
  应用进程，同进程多窗口由共享服务排序
- [ ] 音频/视频自动转写
- [ ] AI 写书或其它写操作工具
- [ ] 在许可证元数据冲突解决前进行打包、发布或重新分发
- [ ] 非 Windows 平台的正式支持声明
