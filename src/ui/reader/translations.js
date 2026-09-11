// Trusted reading-time translation layer.
//
// Translations are matched to chapter blocks by normalized source text, so the
// same runtime serves both the original EPUB XHTML and the synthesized EPUB the
// importer produces for Office/Kindle formats. Inserted nodes are marked with
// `data-moye-translation`; the notes runtime excludes them from its book-text
// index and selection handling. Translation text is written with `textContent`
// only, never as HTML.
(() => {
  "use strict";

  const MARK = "data-moye-translation";
  const ORIGINAL_DISPLAY = "data-moye-original-display";
  const SELECTOR = "p, h1, h2, h3, h4, h5, h6, li, blockquote, td, th";

  let session = "";
  let appliedCount = 0;

  const normalize = (value) => (value || "").replace(/\s+/gu, " ").trim();

  const isCell = (element) =>
    element.tagName === "TD" || element.tagName === "TH";

  const clear = () => {
    for (const node of Array.from(document.querySelectorAll(`[${MARK}]`))) {
      const original = node.__moyeOriginal;
      if (original && original.isConnected) {
        const previous = original.getAttribute(ORIGINAL_DISPLAY);
        if (previous !== null) {
          original.style.display = previous;
          original.removeAttribute(ORIGINAL_DISPLAY);
        }
      }
      node.remove();
    }
    appliedCount = 0;
  };

  // Only the innermost block elements participate, so a list item that merely
  // wraps a paragraph is matched once, on the paragraph.
  const candidateElements = () => {
    const all = Array.from(document.body.querySelectorAll(SELECTOR));
    const parents = new Set();
    for (const element of all) {
      const parent = element.parentElement?.closest(SELECTOR);
      if (parent) parents.add(parent);
    }
    return all.filter((element) => !parents.has(element));
  };

  const toggle = (element, block, divider) => {
    let collapsed = block.getAttribute("data-moye-collapsed") === "1";
    return (event) => {
      if (event) {
        event.preventDefault();
        event.stopPropagation();
      }
      collapsed = !collapsed;
      divider.style.display = collapsed ? "none" : "";
      if (collapsed) {
        if (element.getAttribute(ORIGINAL_DISPLAY) === null) {
          element.setAttribute(ORIGINAL_DISPLAY, element.style.display || "");
        }
        element.style.display = "none";
      } else {
        const previous = element.getAttribute(ORIGINAL_DISPLAY);
        if (previous !== null) {
          element.style.display = previous;
          element.removeAttribute(ORIGINAL_DISPLAY);
        }
      }
      block.setAttribute("data-moye-collapsed", collapsed ? "1" : "0");
    };
  };

  const buildBlock = (element, translated, collapsed) => {
    const block = document.createElement("div");
    block.setAttribute(MARK, "1");
    block.className = "moye-translation-block";
    block.style.cssText = "margin:0 0 0.35em 0;padding:0;";
    block.setAttribute("data-moye-collapsed", collapsed ? "1" : "0");

    const text = document.createElement("div");
    text.className = "moye-translation-text";
    text.setAttribute("dir", "auto");
    text.textContent = translated;
    text.style.cssText = "white-space:pre-wrap;";

    const divider = document.createElement("div");
    divider.className = "moye-translation-divider";
    divider.style.cssText =
      "border-top:1px dashed rgba(120,120,120,0.5);margin:0.35em 0;";

    block.append(text, divider);

    // Hiding a table cell would hide its own translation, so cells stay
    // bilingual and are not click-toggleable.
    if (!isCell(element)) {
      // When "only translation" is the preference, the original is hidden up
      // front; a click still reveals it through the same toggle.
      if (collapsed) {
        element.setAttribute(ORIGINAL_DISPLAY, element.style.display || "");
        element.style.display = "none";
        divider.style.display = "none";
      }
      const onToggle = toggle(element, block, divider);
      text.style.cursor = "pointer";
      text.addEventListener("click", onToggle);
      block.addEventListener("click", (event) => {
        if (event.target === block) onToggle(event);
      });
      block.__moyeOriginal = element;
    }
    return block;
  };

  const insert = (element, block) => {
    if (isCell(element)) {
      element.insertBefore(block, element.firstChild);
    } else {
      element.parentNode.insertBefore(block, element);
    }
  };

  const configure = (payload) => {
    if (!payload || typeof payload !== "object") return;
    session = typeof payload.session === "string" ? payload.session : "";
    clear();
    const entries = Array.isArray(payload.blocks) ? payload.blocks : [];
    if (!entries.length) return;

    // "translation-only" hides the original text until the reader toggles a
    // paragraph; every other value keeps the original visible (bilingual).
    const translateOnly = payload.displayMode === "translation-only";

    const queues = new Map();
    for (const entry of entries) {
      if (!entry || typeof entry.translated !== "string") continue;
      const key = normalize(entry.source);
      if (!key) continue;
      if (!queues.has(key)) queues.set(key, []);
      queues.get(key).push(entry.translated);
    }
    if (!queues.size) return;

    for (const element of candidateElements()) {
      const queue = queues.get(normalize(element.textContent));
      if (!queue || !queue.length) continue;
      const translated = queue.shift();
      if (!translated) continue;
      insert(element, buildBlock(element, translated, translateOnly && !isCell(element)));
      appliedCount += 1;
    }
  };

  const reset = () => {
    session = "";
    clear();
  };

  window.moyeTranslations = Object.freeze({
    configure,
    clear: reset,
    applied: () => appliedCount,
    session: () => session,
  });
})();
