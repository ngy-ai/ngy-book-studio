# 不可破坏的 WebView、窗口与内容安全约束

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 不可破坏的 WebView 与窗口约束

1. `src/main.rs` 必须在 GPUI 启动任何工作线程前设置
   `GPUI_DISABLE_DIRECT_COMPOSITION=1`，并在创建应用前完成 WebView2 Runtime 探测。
   应用启动时调用 `gpui_kit::init(cx)`；每个使用 `gpui-kit` 组件层的窗口必须以
   `Root::new(...)` 作为第一层视图。
2. Reader、Editor 和 PDF Reader 的 Wry 子 WebView 必须通过
   `build_as_child_async(...).await` 异步创建。等待过程位于 GPUI entity/state 借用
   区间之外，完成后再重新进入 context 附加实体，避免 Windows 消息泵重入借用 panic。
3. 捕获的 `Win32WindowHandle` 只是父 HWND 的借用值。异步构建尚未结束时，必须通过
   现有 build gate 否决关闭并延迟处理；不要假设 detached task 随窗口自动取消。
4. Reader/Editor/PDF Reader 关闭时，先从状态和渲染帧移除并隐藏子 WebView，等待
   entity 释放，再调用现有原生窗口移除 helper。不要让默认 `WM_CLOSE` 抢先销毁父 HWND。
  Editor 关闭先询问保存并关闭、不保存或取消；只有确认保存才进入快照和写入流程，
   放弃修改直接安全释放，取消保留输入与 AI 状态。初始化或已接受写入尚未完成时先
   等待；确认框期间若旧请求开始写入，放弃关闭也须等其结果回填，不得另行触发保存。
   Reader/PDF Reader 还必须等最终稳定 locator 的阅读进度写入成功；写入失败时保持窗口
   打开并允许再次关闭重试，不能把“后台 worker 已退出”当作持久化成功。
5. 图书库主窗口关闭先询问“退出软件 / 继续运行”；取消前不得最小化、取消 AI 或
   改变窗口关闭状态。确认后通过 `on_window_close` 注册的原关闭回调逐个关闭子窗口，
   编辑取消、快照/保存/阅读进度/学习持久化失败须调用 `cancel_application_exit` 撤销
   本次退出。五个 AI 设置窗口各自在已经开始保存时等待结果（按 `PendingOperation::Saving` 判断，上界是界面的
   `SAVE_WAIT_LIMIT`：保存被后台任务堵住时最多等这么久，之后窗口按已结束处理）。子窗全部移除后，主窗口再等待已接受的
   导入、创建、分组、移动或删除 mutation 完成，最后安全移除并让 GPUI 自然退出。
   不调用 `App::quit()`/`cx.quit()` 绕过 close veto；`on_window_closed` 订阅保存在全局
   协调器中并 defer 推进。实际开窗前检查退出状态，防止迟到的异步打开留下孤立窗口。
6. EPUB 后续目录、上下章和搜索跳转在 `load_url` 前必须经过
   `OpenedBook::navigation_url_for_href`；Windows 不会自动复用 builder 阶段协议映射。
7. Windows Debug 主线程只有约 1 MiB 栈。Editor 这类大型 GPUI `Render` 不得重新
   合并为单个巨型 builder 表达式；较大的区域应通过非内联 helper 单独构造并在边界
   type-erase，否则编译器可能为一次渲染预留数百 KiB 栈帧并在首帧溢出。
8. Wry 的 initialization script 在 XHTML 根元素建立前执行。ProseMirror bundle 的
   第三方模块会在求值阶段访问 `document.documentElement`，所以必须保留构建脚本中
   对整个 bundle 的 `DOMContentLoaded` 外层门禁；只在 TypeScript 入口函数里等待
   DOM 仍然太晚。修改后同时运行前端 typecheck 和 bundle 一致性检查。
9. 固定高度的横向工具栏使用带稳定 ID 的 GPUI `overflow_x_scroll()`，不要把
   `overflow_x_scrollbar()` 直接套在承载按钮的 `h_flex()` 上：0.5.1 的包装器会把
   原元素样式转移到 `size_full()` 外壳并清空内部布局。0.6 已改为保留原元素作为
   滚动容器，但那两种语义仍然不要混用，工具栏高度与滚动条行为改完要人工确认。
10. 富文本 IPC 返回完整的可信 XHTML 页面壳，但只有 `<body>` 属于可编辑正文。更新
    统一 AST 前先提取该元素再清洗解析；不要把含 `<head><title>` 的整页交给片段
    清洗器，否则移除标签后留下的标题文本可能进入正文并在保存时重复。
11. HTML 源码不是 XHTML。编辑器首次投影及源码刷新都用 `markup::serialize_xhtml`
    从已清洗的 AST 生成展示片段，再放入 XHTML 壳；通过 HTML DOM 处理空元素、
    布尔属性与实体并按 XML 转义，不能只替换 `<img>` 或放宽前端严格解析。
    不为展示改写数据库源码，也不能再次清洗并误删受控任务勾选框或媒体。
    富文本 `body: null` 回执与已接纳章节一致时，只释放精确匹配的待处理动作，不重建
    AST；其附带的是展示壳，重新解析会把正文变成 RawHtml 并丢失结构化媒体引用。
    若前次正文解析失败，回执仍携带未接纳正文，必须重试校验并继续阻止保存和切章，
    直到正文修复；不能把前端的 unchanged 标记当作宿主已经接纳该快照的证据。
12. PDF.js viewer 的可见字形画在 canvas 上，`.textLayer` 的 span 是
    `color: transparent` 的覆盖层。宿主注入的皮肤覆盖规则（`PDF_APPEARANCE_RULES`，
    见 `src/ui/pdf_reader/viewer.rs`）只能画半透明层：不透明的底色会画在 canvas 之上，
    把正文整块盖住（选中一段文字后那里只剩一块纯色）。选中底色因此是叠了 alpha 的
    强调色，与 `Palette::selection()` 同一档，不要写回不透明的 `--ngy-accent-soft`。
14. Reader、Editor 与 PDF Reader 的正文是原生子窗口 WebView，它画在 GPUI 画布之上：
    任何落进正文区域的 GPUI 浮层（`Popover`、`PopupMenu`、tooltip，含 `deferred` 层）
    都会被它整块盖住，不会报错，只是用户看不到。窗口顶部能完整显示浮层的只有工具栏
    （阅读窗口 66px）加进度条（3px）与正文卡片上边距（12px）这条固定高度的缝。工具栏
    上的弹层因此要么整体留在那条缝里（例如阅读窗口的语速档位：单行横排 +
    `Anchor::BottomLeft`，被窗口边缘钳制后不会伸进正文），要么改用独立窗口或 WebView2
    原生菜单；不要做成竖向下拉再指望它显示在正文上。
15. Windows 主线程栈默认只有 1 MiB，而组件层的 `request_layout`/`paint` 沿元素树递归，
    每个节点还带着滚动容器、文本样式、交互层的包装，比旧版更深。`build.rs` 为 bin
    目标加的 `/STACK:16777216` 是必需的：删掉它，图书库窗口会在后台任务触发重绘时以
    `STATUS_STACK_OVERFLOW`（0xc00000fd）崩在 `render_library` 的布局递归里。需要看
    崩溃栈时用 `cdb -o -g -G -c "g; kb 60; q" target\debug\ngy-book-studio.exe`。

## 不可破坏的内容安全约束

- EPUB Reader 只提供 manifest 声明资源；保留编码路径穿越、非法路径/MIME 拒绝、
  内部 origin 白名单、CSP、`nosniff`、禁脚本/联网/外部导航/新窗口/下载和 incognito。
- PDF.js 必须完全本地加载，禁止 PDF JavaScript/XFA 和网络请求；协议路由只接受固定
  资产与当前 PDF，不得映射任意主机文件。
- Editor 的宿主初始化脚本是可信桥接，不代表允许书内脚本。IPC 必须校验 origin、
  href、session/revision/request-id、Ready/请求状态和大小上限。保存、切章、导出、
  确认保存后关闭和 AI 引用必须取得完全匹配的快照，旧页面回调不能覆盖新章节。
- 打开 Editor 时只从当前 revision 的 `BookDocument` AST 生成会话投影，不读取或重新
  打包整本原格式容器，也不回退提供原件中的 CSS/字体/任意路径资源。持久媒体只在
  协议请求时经 `AppServices` 后台读取；关闭时先封闭协议响应屏障，再释放 WebView，
  禁止迟到任务向已销毁的 Wry responder 回调。
- 富文本 shell 与不可信正文保持 origin/能力隔离。HTML 中仅允许白名单音视频元素；
  RawHtml 必须清洗，外链、事件处理器和主动嵌入不得进入预览。
- 不要随意提高归档、正文、IPC、视觉页面或模型上下文大小上限。调整时补上限内、
  越界、取消与资源耗尽测试并说明风险。