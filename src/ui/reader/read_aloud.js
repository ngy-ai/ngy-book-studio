// 朗读的页面侧：段落 hover 出现小喇叭，点击从该段开始连续朗读。
//
// 与译文层同一条边界：页面只说「读什么」（当前显示模式下的段落文字），宿主负责
// 合成与播放（Windows 系统语音），再把「正在读哪一段」推回来做高亮。
//
// 触发方式不用单击段落：正文的 pointerup 已经被笔记占用（拖选后的选区工具栏与
// 点击划线弹卡片），单击朗读会和它们打架。
(() => {
  "use strict";

  const XHTML = "http://www.w3.org/1999/xhtml";
  const SVG_NAMESPACE = "http://www.w3.org/2000/svg";
  const MARK = "data-ngy-translation";
  // 与译文层一致：只朗读最内层的块。表格单元格（td/th）不在此列——读表格没有
  // 意义，代码块同样跳过。
  const BLOCK_SELECTOR = "p, h1, h2, h3, h4, h5, h6, li, blockquote";
  const ACTIVE_CLASS = "ngy-read-aloud-active";
  const BUTTON_CLASS = "ngy-read-aloud-button";
  const BUTTON_STYLE = "display:inline-block!important;width:1em!important;height:1em!important;" +
    "margin:0 0 0 0.35em!important;padding:0!important;border:0!important;background:transparent!important;" +
    "color:inherit!important;opacity:0!important;pointer-events:none!important;cursor:pointer!important;" +
    "vertical-align:-0.12em!important;font:inherit!important;line-height:1!important;" +
    "transition:opacity 0.12s ease-out;";
  const ACTIVE_STYLE = `.${ACTIVE_CLASS}{background-color:rgba(255,214,120,0.38)!important;` +
    "border-radius:3px;}";
  const SPEAKER_STROKE =
    "M11 5 6 9H3v6h3l5 4V5z M15.5 8.5a5 5 0 0 1 0 7 M18.5 6a9 9 0 0 1 0 12";

  const create = (name) => document.createElementNS(XHTML, name);

  const speakerIcon = () => {
    const svg = document.createElementNS(SVG_NAMESPACE, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("width", "1em");
    svg.setAttribute("height", "1em");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    // 图标只有路径、没有文本节点：插进块里不会改变 textContent，也不会被复制。
    for (const data of SPEAKER_STROKE.split(" M")) {
      const path = document.createElementNS(SVG_NAMESPACE, "path");
      path.setAttribute("d", data.startsWith("M") ? data : `M${data}`);
      svg.append(path);
    }
    return svg;
  };

  // 只取最内层的块。译文层不算朗读对象：它给每个块复制了一份与源同名的文本
  // 元素（`<p>` 的译文还是 `<p>`），不排除就会把同一段读两遍、索引也整体后移。
  const candidateBlocks = () => {
    const all = Array.from(document.body.querySelectorAll(BLOCK_SELECTOR))
      .filter(
        (element) => !element.closest(`script,style,noscript,template,pre,code,[${MARK}]`),
      );
    const parents = new Set();
    for (const element of all) {
      const parent = element.parentElement?.closest(BLOCK_SELECTOR);
      if (parent) parents.add(parent);
    }
    return all.filter((element) => !parents.has(element));
  };

  /// 块对应的译文层：普通块在相邻位置，列表项在块内部。
  const layerOf = (element) => {
    const sibling = element.previousElementSibling;
    if (sibling && sibling.hasAttribute(MARK)) return sibling;
    const child = element.querySelector(`:scope > [${MARK}]`);
    return child || null;
  };

  /// 当前显示模式下的文字。有译文层就朗读译文——译文模式与双语模式显示的是译文，
  /// 原文模式下译文层已被清除，于是自然读回原文。
  const displayText = (element) => {
    const layer = layerOf(element);
    if (layer) {
      const text = layer.querySelector(".ngy-translation-text");
      if (text && text.textContent.trim()) return text.textContent;
    }
    const clone = element.cloneNode(true);
    clone.querySelectorAll(`[${MARK}], .${BUTTON_CLASS}`).forEach((node) => node.remove());
    return clone.textContent || "";
  };

  const collect = () =>
    candidateBlocks()
      .map((element) => ({ element, text: displayText(element).trim() }))
      .filter((item) => item.text);

  const post = (message) => {
    if (window.ipc && typeof window.ipc.postMessage === "function") {
      window.ipc.postMessage(JSON.stringify(message));
    }
  };

  const ensureButtons = () => {
    for (const block of candidateBlocks()) {
      if (block.querySelector(`:scope > .${BUTTON_CLASS}`)) continue;
      const button = create("button");
      button.type = "button";
      button.className = BUTTON_CLASS;
      button.setAttribute("aria-label", "从这里开始朗读");
      button.setAttribute("title", "从这里开始朗读");
      button.style.cssText = BUTTON_STYLE;
      button.append(speakerIcon());
      button.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        startWith(block);
      });
      // 悬停在本段时淡入：按钮本身 opacity:0 且不吃指针事件，所以由块的悬停驱动。
      block.addEventListener("mouseenter", () => {
        button.style.setProperty("opacity", "0.5", "important");
        button.style.setProperty("pointer-events", "auto", "important");
      });
      block.addEventListener("mouseleave", () => {
        button.style.setProperty("opacity", "0", "important");
        button.style.setProperty("pointer-events", "none", "important");
      });
      block.append(button);
    }
  };

  const startWith = (block) => {
    ensureButtons();
    const list = collect();
    const index = list.findIndex((item) => item.element === block);
    if (index < 0) return;
    // 段落 id 就是这一轮列表里的下标：宿主只按顺序朗读，不需要别的身份。
    post({
      type: "read_aloud_start",
      start_block: String(index),
      blocks: list.map((item, position) => ({ id: String(position), text: item.text })),
    });
    highlight(String(index));
  };

  const startFromViewport = () => {
    ensureButtons();
    const list = collect();
    if (!list.length) return;
    const target = list.find((item) => item.element.getBoundingClientRect().bottom > 8) || list[0];
    startWith(target.element);
  };

  const clear = () => {
    document
      .querySelectorAll(`.${ACTIVE_CLASS}`)
      .forEach((node) => node.classList.remove(ACTIVE_CLASS));
  };

  /// 高亮当前朗读段并滚到可视区。只切换一个可逆的类，不动正文节点、文本与选区。
  const highlight = (id) => {
    clear();
    const index = Number(id);
    if (!Number.isInteger(index) || index < 0) return;
    const item = collect()[index];
    if (!item) return;
    item.element.classList.add(ACTIVE_CLASS);
    item.element.scrollIntoView({ block: "nearest" });
  };

  const install = () => {
    if (document.querySelector(`style[data-ngy-read-aloud]`)) return;
    const style = create("style");
    style.setAttribute("data-ngy-read-aloud", "1");
    style.textContent = ACTIVE_STYLE;
    document.head.append(style);
    ensureButtons();
  };

  window.ngyReadAloud = { startFromViewport, highlight, clear, refresh: ensureButtons };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", install, { once: true });
  } else {
    install();
  }
})();
