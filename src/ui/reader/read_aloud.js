// 朗读的页面侧：段落 hover 出现小喇叭，点击从该段开始连续朗读。
//
// 与译文层同一条边界：页面只说「读什么」（当前显示模式下的段落文字），宿主负责
// 合成与播放（Windows 系统语音），再把「正在读哪一段」推回来做高亮。
//
// 小喇叭、高亮与「从视口开始」的判定都落在**读者看得见的那份文字**上：双语、原文
// 模式以及含媒体而保持双语的段落是正文块自己；「译文」模式下原文整块被隐藏（连块里
// 的按钮一起），可见的是译文层的译文，三者因此都落到译文末尾。译文层与折叠状态会
// 被显示方式切换、载荷重推重建，所以 `ngyTranslations` 在那之后叫一次 `refresh()`，
// 由这里把同一枚按钮搬到新的可见文字上（一段永远只有一枚，不新建）。
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
  const TRANSLATION_TEXT = ".ngy-translation-text";
  // 译文层用它说明「原文已经折叠」：这时可见的文字是译文，朗读对象要跟着走。
  const COLLAPSED = "data-ngy-collapsed";
  // 列表项与表格单元格里承载原文的 `display:contents` 包装（译文层写的标记）。
  const ORIGINAL = "data-ngy-translation-original";
  const ACTIVE_CLASS = "ngy-read-aloud-active";
  const BUTTON_CLASS = "ngy-read-aloud-button";
  const BUTTON_STYLE = "display:inline-block!important;width:1em!important;height:1em!important;" +
    "margin:0 0 0 0.35em!important;padding:0!important;border:0!important;background:transparent!important;" +
    "color:inherit!important;opacity:0!important;pointer-events:none!important;cursor:pointer!important;" +
    "vertical-align:-0.12em!important;font:inherit!important;line-height:1!important;" +
    "transition:opacity 0.12s ease-out;";
  const HIGHLIGHT_BACKGROUND = "rgba(255,214,120,0.38)";
  const HIGHLIGHT_RADIUS = "3px";
  const ACTIVE_STYLE = `.${ACTIVE_CLASS}{background-color:${HIGHLIGHT_BACKGROUND}!important;` +
    `border-radius:${HIGHLIGHT_RADIUS};}`;
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

  /// 这一段里读者真正看得见的那份文字：「译文」模式把原文整块隐藏（`display:none`，
  /// 连块末尾的按钮一起），可见的只有译文层的译文——小喇叭、高亮与「从视口开始」的
  /// 判定都得按它算；双语、原文模式以及含媒体而保持双语的段落是原文自己。
  ///
  /// 列表项与表格单元格的原文被译文层搬进一个 `display:contents` 的包装里（这样才
  /// 能只藏原文）。包装不生成盒子，所以原文宿主是它、不是块本身：按钮挂到块上会落到
  /// 包装外面（DOM 顺序变了），悬停也由块来接（包装收不到自己的 `mouseenter`）。
  const visibleHost = (block) => {
    const layer = layerOf(block);
    if (layer && layer.getAttribute(COLLAPSED) === "1") {
      return layer.querySelector(TRANSLATION_TEXT) || block;
    }
    return block.querySelector(`:scope > [${ORIGINAL}]`) || block;
  };

  /// 当前显示模式下的文字。有译文层就朗读译文——译文模式与双语模式显示的是译文，
  /// 原文模式下译文层已被清除，于是自然读回原文。
  const displayText = (element) => {
    const layer = layerOf(element);
    if (layer) {
      const text = layer.querySelector(TRANSLATION_TEXT);
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

  // 段落 -> 它那枚按钮。按钮会在「正文块」与「译文层译文」之间搬家（显示方式一切换、
  // 载荷一重推，译文层整体重建，旧层里的按钮随之离开文档），搬的是同一个节点，
  // 所以一段永远只有一枚；`refresh()` 之后按当前可见文字重新挂一次。
  const buttons = new Map();
  // 悬停驱动每个承载元素只绑一次：同一个按钮会在两个承载元素之间来回。
  const hoverBound = new WeakSet();

  const setShown = (button, shown) => {
    button.style.setProperty("opacity", shown ? "0.5" : "0", "important");
    button.style.setProperty("pointer-events", shown ? "auto" : "none", "important");
  };

  // 悬停在承载元素上时淡入：按钮本身 opacity:0 且不吃指针事件，所以由承载它的
  // 那份可见文字驱动（鼠标移到按钮上仍在该元素内部，不会触发 mouseleave）。
  const bindHover = (host, button) => {
    if (hoverBound.has(host)) return;
    hoverBound.add(host);
    host.addEventListener("mouseenter", () => setShown(button, true));
    host.addEventListener("mouseleave", () => setShown(button, false));
  };

  const createButton = (block) => {
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
    return button;
  };

  const ensureButtons = () => {
    for (const block of candidateBlocks()) {
      const host = visibleHost(block);
      let button = buttons.get(block);
      if (!button) {
        button = createButton(block);
        buttons.set(block, button);
      }
      // 位置没变就不动它（按钮因此留在译文最后一个字之后）；承载元素换了就搬过去，
      // 旧译文层已被摘掉时（按钮脱离文档）也同样挂回来。
      if (button.parentElement !== host) host.append(button);
      // 悬停由块与承载元素一起接：承载元素是 `display:contents` 包装时它自己不生成
      // 盒子、收不到 `mouseenter`，指针事件落在块上；原文被隐藏时块又没有盒子，只能靠
      // 译文层的悬停。
      bindHover(block, button);
      if (host !== block) bindHover(host, button);
    }
    // 章节换了以后旧块不存在了：记着的按钮随块一起丢，别越攒越多。
    for (const block of [...buttons.keys()]) {
      if (!block.isConnected) buttons.delete(block);
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
    // 视口判定按可见文字算：「译文」模式下原文块没有盒子（`display:none`），拿它比会
    // 把每一段都判成不可见、永远从章首开始读。
    const target =
      list.find((item) => visibleHost(item.element).getBoundingClientRect().bottom > 8) || list[0];
    startWith(target.element);
  };

  // 高亮过的元素 -> 它原来的内联背景与圆角，清除时原样还回去。
  const highlighted = new Map();

  const restoreInline = (host, name, value, priority) => {
    if (value) host.style.setProperty(name, value, priority);
    else host.style.removeProperty(name);
  };

  const clear = () => {
    for (const [host, previous] of highlighted) {
      restoreInline(host, "background-color", previous.color, previous.colorPriority);
      restoreInline(host, "border-radius", previous.radius, previous.radiusPriority);
      host.classList.remove(ACTIVE_CLASS);
    }
    highlighted.clear();
    document
      .querySelectorAll(`.${ACTIVE_CLASS}`)
      .forEach((node) => node.classList.remove(ACTIVE_CLASS));
  };

  /// 高亮当前朗读段并滚到可视区。只改可逆的样式，不动正文节点、文本与选区。
  ///
  /// 落在正文块上时 `.ngy-read-aloud-active` 就够了；落在译文层的译文上时不够——译文层
  /// 把原文的 computed style 逐条写成内联 `!important`（连 `background-color:
  /// rgba(0, 0, 0, 0)` 也在内），内联的 `!important` 压过样式表里的高亮规则，所以要
  /// 同时写一条内联颜色，清除时按记下的旧值还原。
  const highlight = (id) => {
    clear();
    const index = Number(id);
    if (!Number.isInteger(index) || index < 0) return;
    const item = collect()[index];
    if (!item) return;
    const host = visibleHost(item.element);
    if (!highlighted.has(host)) {
      highlighted.set(host, {
        color: host.style.getPropertyValue("background-color"),
        colorPriority: host.style.getPropertyPriority("background-color"),
        radius: host.style.getPropertyValue("border-radius"),
        radiusPriority: host.style.getPropertyPriority("border-radius"),
      });
      host.style.setProperty("background-color", HIGHLIGHT_BACKGROUND, "important");
      host.style.setProperty("border-radius", HIGHLIGHT_RADIUS, "important");
    }
    host.classList.add(ACTIVE_CLASS);
    host.scrollIntoView({ block: "nearest" });
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
