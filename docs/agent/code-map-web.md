# 代码地图：前端源码与构建产物

> 本文件由根 `AGENTS.md` 拆分而来；全局约束与文档索引见根 `AGENTS.md`。

## 模块

- `web/editor/` 与 `assets/editor/`：固定版本 ProseMirror 源码、lockfile 和提交的构建
  产物；普通 Cargo 构建不运行 Node。
- `web/pdf/` 与 `assets/pdfjs/`：固定版本 PDF.js shell、lockfile、清单和提交的本地
  资产；不得改为 CDN 或运行时联网获取。`web/pdf/src/viewer.mjs` 是上下连续滚动的实现：
  页列一次性布局，`IntersectionObserver` 按需绘制、远离阅读位置后回收为占位页；滚动
  停止约 150ms 才上报 `ngy-pdf-page-changed`，并复用最近一次 `ngy-pdf-go-to` 的
  `requestId` 以保持宿主过期请求拒绝语义。读取 URL 的 `compact=1` 后给 `<html>` 设置
  `data-pdf-compact`：这是“PDF 紧凑阅读”唯一的页间距契约，宿主对已打开窗口也用同一
  属性做实时更新。改动 `web/pdf/src/` 后必须在 `web/pdf` 执行
  `npm ci` 与 `node scripts/build.mjs` 重新生成 `assets/pdfjs/viewer.html`、
  `viewer.mjs` 与 `manifest.json`，并让 `node scripts/check-bundle.mjs` 通过。

## 前端构建产物

只有修改 `web/editor/`、`web/pdf/` 或对应依赖时才需要 Node.js 22+。前端统一使用
`npm`；两个目录各自的 `package-lock.json` 是唯一权威依赖锁，必须与 `package.json`
和确定性构建产物一起提交。不要使用或生成 `pnpm-lock.yaml`、`yarn.lock` 等第二套
锁文件。最终 Rust 验证前检查构建产物没有陈旧：

```powershell
Push-Location web/editor
npm ci
npm run check
npm run build
npm run check
Pop-Location

Push-Location web/pdf
npm ci
npm run check
npm run build
npm run check
Pop-Location
```

`web/editor` 的 `check` 同时执行 TypeScript 类型检查和 bundle 一致性检查；
`web/pdf` 的 `check` 校验 PDF.js 版本、清单摘要、CSP、本地资源与禁用 eval/XFA 等
安全契约。不要手工编辑生成的 `assets/pdfjs/routes.rs`、manifest 或构建 bundle。