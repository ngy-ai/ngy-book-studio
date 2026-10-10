// PDF 朗读的页面侧：从 PDF.js 文字层收集段落，并把「正在读哪一段」高亮。
//
// 与 EPUB 阅读器同一模式：页面只说「读什么」（已经绘制出来的页面文字层里的
// 文字），宿主负责合成与播放，再把「正在读哪一段」推回来做高亮。差别在文字
// 层的形状：PDF 的文字层是绝对定位的行片段（每个文本项一个 span，行末一个
// <br>），没有段落语义，所以这里按行距把相邻行合并成段，并给一段设字符上限，
// 让朗读与高亮的粒度稳定。
//
// 只覆盖已经绘制的页面：连续滚动按需渲染，未绘制的页没有文字层，读到已绘制
// 范围的末尾就停（与笔记「只有已绘制的页面会显示标记」是同一条限制）。读到更
// 后面，滚动带出新的页面后再点一次朗读即可。
(() => {
  "use strict";

  const ACTIVE_CLASS = "ngy-read-aloud-active";
  // 一段最多多少字符：超过就按句末标点切开（PDF 没有段落语义，长度是唯一
  // 可靠的额外边界）。与宿主侧 8 KiB 的单段上限相比留了充足余量。
  const MAX_BLOCK_CHARS = 480;
  // 相邻行的间隙超过行高的这个比例就视为新段。
  const PARAGRAPH_GAP = 0.6;
  const ACTIVE_STYLE =
    `.${ACTIVE_CLASS}{background-color:rgba(255,214,120,0.38)!important;` +
    "border-radius:3px;}";
  const SENTENCE_END = /[。！？!?;；.…]["'”’）)\]]*$/;

  const post = (message) => {
    if (window.ipc && typeof window.ipc.postMessage === "function") {
      window.ipc.postMessage(JSON.stringify(message));
    }
  };

  // 已绘制的页面，按页序。
  const renderedLayers = () =>
    Array.from(document.querySelectorAll(".pdf-page[data-rendered='true'] .textLayer"))
      .map((layer) => ({ layer, page: Number(layer.closest(".pdf-page")?.dataset.page) || 0 }))
      .sort((a, b) => a.page - b.page);

  const boxOf = (spans) => {
    let top = Infinity;
    let bottom = -Infinity;
    for (const span of spans) {
      const rect = span.getBoundingClientRect();
      if (rect.height === 0) continue;
      top = Math.min(top, rect.top);
      bottom = Math.max(bottom, rect.bottom);
    }
    if (!Number.isFinite(top)) return { top: 0, bottom: 0, height: 0 };
    return { top, bottom, height: bottom - top };
  };

  // 一行：由 <br> 分开的叶子 span（TextLayer 在每个带 EOL 的项之后放一个）。
  // markedContent 包装 span 要递归进去，否则整块文字会被漏掉。
  const linesOf = (layer) => {
    const lines = [];
    let current = [];
    const flush = () => {
      if (!current.length) return;
      const box = boxOf(current);
      if (box.height > 0) lines.push({ spans: current, ...box });
      current = [];
    };
    const walk = (node) => {
      for (const child of node.childNodes) {
        if (child.nodeType !== Node.ELEMENT_NODE) continue;
        const tag = child.tagName;
        if (tag === "BR") {
          flush();
        } else if (tag === "SPAN" && !child.querySelector("span")) {
          current.push(child);
        } else {
          walk(child);
        }
      }
    };
    walk(layer);
    flush();
    return lines;
  };

  // 行 → 段：行距小的算同一段；行序在几何上跳回去（多栏或定位包装，间隙为负）
  // 时另起一段，不要把两处文字粘在一起。
  const paragraphsOf = (lines) => {
    const paragraphs = [];
    let current = null;
    for (const line of lines) {
      const gap = current ? line.top - current.bottom : Infinity;
      const height = current ? Math.max(current.height, line.height) : line.height;
      if (!current || gap > height * PARAGRAPH_GAP || gap < 0) {
        if (current) paragraphs.push(current);
        current = { lines: [], spans: [], top: line.top, bottom: line.bottom, height: line.height };
      }
      current.lines.push(line);
      current.spans.push(...line.spans);
      current.bottom = line.bottom;
      current.height = Math.max(current.height, line.height);
    }
    if (current) paragraphs.push(current);
    return paragraphs;
  };

  // 行与行之间要不要补空格：两侧都是拉丁文字/开括号时才补，中文换行不加。
  const needsSpace = (previous, next) =>
    /[0-9A-Za-z,.;:!?)\]]$/.test(previous) && /^[0-9A-Za-z(\[]/.test(next);

  const textOfLines = (lines) => {
    let text = "";
    for (const line of lines) {
      const piece = line.spans.map((span) => span.textContent || "").join("").trim();
      if (!piece) continue;
      if (text && needsSpace(text, piece)) text += " ";
      text += piece;
    }
    return text.trim();
  };

  // 超长段按句末标点切成多块；每块仍引用同一组 span（高亮粒度不变）。
  const splitBlocks = (text) => {
    if (text.length <= MAX_BLOCK_CHARS) return [text];
    const blocks = [];
    let rest = text;
    while (rest.length > MAX_BLOCK_CHARS) {
      let cut = 0;
      const lowest = Math.floor(MAX_BLOCK_CHARS * 0.4);
      for (let index = MAX_BLOCK_CHARS; index > lowest; index -= 1) {
        if (SENTENCE_END.test(rest.slice(0, index))) {
          cut = index;
          break;
        }
      }
      if (cut <= 0) cut = MAX_BLOCK_CHARS;
      // 不要在 UTF-16 代理对中间切开。
      if (cut < rest.length && /[\uD800-\uDBFF]/.test(rest[cut - 1])) cut += 1;
      blocks.push(rest.slice(0, cut).trim());
      rest = rest.slice(cut);
    }
    if (rest.trim()) blocks.push(rest.trim());
    return blocks.filter((block) => block);
  };

  let list = [];
  let activeSpans = [];

  const collect = () => {
    const collected = [];
    for (const { page, layer } of renderedLayers()) {
      for (const paragraph of paragraphsOf(linesOf(layer))) {
        const text = textOfLines(paragraph.lines);
        if (!text) continue;
        for (const piece of splitBlocks(text)) {
          collected.push({ page, spans: paragraph.spans, text: piece });
        }
      }
    }
    return collected;
  };

  const clear = () => {
    for (const span of activeSpans) span.classList.remove(ACTIVE_CLASS);
    activeSpans = [];
  };

  // 高亮当前朗读段并滚到可视区。只切换一个可逆的类，不动文字层节点。
  const highlight = (id) => {
    clear();
    const index = Number(id);
    if (!Number.isInteger(index) || index < 0) return;
    const item = list[index];
    if (!item) return;
    let first = null;
    for (const span of item.spans) {
      // 页面可能已经被回收卸载：跳过失效节点，不要抛错。
      if (!span.isConnected) continue;
      span.classList.add(ACTIVE_CLASS);
      activeSpans.push(span);
      if (!first) first = span;
    }
    first?.scrollIntoView({ block: "nearest" });
  };

  // 从 `list` 里的一段开始：列表必须是同一次收集的结果，重新收集会换掉对象
  // 身份，`indexOf` 找不到目标，消息就永远发不出去。
  const begin = (target) => {
    const index = list.indexOf(target);
    if (index < 0) return;
    post({
      type: "read_aloud_start",
      start_block: String(index),
      blocks: list.map((item, position) => ({ id: String(position), text: item.text })),
    });
    highlight(String(index));
  };

  // 工具栏的「朗读」：从视口里第一段还看得见的文字开始。
  const startFromViewport = () => {
    list = collect();
    if (!list.length) return;
    const target =
      list.find((item) => {
        const span = item.spans.find((candidate) => candidate.isConnected);
        return span && span.getBoundingClientRect().bottom > 8;
      }) || list[0];
    begin(target);
  };

  const install = () => {
    if (document.querySelector("style[data-ngy-read-aloud]")) return;
    const style = document.createElement("style");
    style.setAttribute("data-ngy-read-aloud", "1");
    style.textContent = ACTIVE_STYLE;
    document.head.append(style);
  };

  window.ngyPdfReadAloud = { startFromViewport, highlight, clear };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", install, { once: true });
  } else {
    install();
  }
})();
