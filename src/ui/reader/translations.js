// Trusted reading-time translation layer. The model returns text leaves only;
// formatting always comes from the current book DOM, never from model markup.
(() => {
  "use strict";

  const MARK = "data-moye-translation";
  const XHTML = "http://www.w3.org/1999/xhtml";
  const SELECTOR = "p, h1, h2, h3, h4, h5, h6, li, blockquote, td, th";
  const SKIPPED = new Set(["script", "style", "noscript", "template"]);
  const PRESERVED = new Set(["code", "pre"]);
  const MEDIA = "img,picture,svg,math,video,audio,canvas,iframe,object,embed,input,select,textarea,button";
  const INLINE_TAGS = new Set([
    "span", "b", "strong", "i", "em", "u", "s", "strike", "del", "ins",
    "sub", "sup", "code", "pre", "br", "small", "mark", "kbd", "samp",
    "var", "abbr", "cite", "q", "ruby", "rt", "rp", "bdi", "bdo", "time",
  ]);
  // All copied properties are inert: no URLs, generated content, positioning,
  // event handlers, book IDs, classes or link targets enter the new subtree.
  const TEXT_STYLES = [
    "color", "background-color", "font-family", "font-size", "font-weight",
    "font-style", "font-stretch", "font-variant", "line-height", "letter-spacing",
    "word-spacing", "text-align", "text-indent", "text-transform", "direction",
    "unicode-bidi", "white-space", "vertical-align", "text-decoration-line",
    "text-decoration-style", "text-decoration-color", "text-decoration-thickness",
    "text-underline-offset", "overflow-wrap", "word-break",
  ];
  const BOX_STYLES = [
    "margin-top", "margin-right", "margin-bottom", "margin-left",
    "padding-top", "padding-right", "padding-bottom", "padding-left",
    ...["top", "right", "bottom", "left"].flatMap((side) =>
      ["width", "style", "color"].map((part) => `border-${side}-${part}`)),
  ];

  let session = "";
  const applied = [];
  // Applied layer element -> its state, so a selection made on a translation can
  // be resolved back to the original text it was built from.
  const layerStates = new Map();
  const normalize = (value) => (typeof value === "string" ? value : "").replace(/\s+/gu, " ").trim();
  // Unicode `Cf` format characters that ECMAScript `\s` does not cover. A leaf
  // made only of these (or of whitespace) has nothing to translate: the model
  // answers it with blanks and the whole block would be rejected. This list
  // must stay identical to `translation::has_visible_text` in Rust, otherwise
  // the two segment lists drift apart and the whole block keeps the original.
  const INVISIBLE = /[\u00ad\u0600-\u0605\u061c\u06dd\u070f\u0890-\u0891\u08e2\u180e\u200b-\u200f\u202a-\u202e\u2060-\u2064\u2066-\u206f\ufff9-\ufffb\u{110bd}\u{110cd}\u{13430}-\u{1343f}\u{1bca0}-\u{1bca3}\u{1d173}-\u{1d17a}\u{e0001}\u{e0020}-\u{e007f}]/gu;
  const translatable = (value) =>
    typeof value === "string" && normalize(value.replace(INVISIBLE, "")) !== "";
  // Code shape, mirroring `looks_like_source_code` in `markup.rs`: a block whose
  // own lines are statements is code even when the book never wrapped it in
  // `pre`/`code`. Prose never matches (full-width `；`/`：` are other characters
  // and a bare `=` is not a signal), and a false positive only keeps that block
  // in its original language. Keep these tables identical to the Rust ones.
  const CODE_LINE_ENDINGS = [";", "{", "}"];
  const CODE_LINE_PREFIXES = ["//", "/*", "*/", "#!", "#include", "#define", "#pragma", "<!--"];
  const CODE_OPERATORS = [
    "=>", "->", "::", ":=", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "</", "/>",
  ];
  const trimCodeLine = (value) =>
    value.replace(/^[\s\u200b\ufeff\u00ad]+|[\s\u200b\ufeff\u00ad]+$/gu, "");
  const isCodeLine = (value) => {
    const line = trimCodeLine(value);
    if (!line) return false;
    return CODE_LINE_ENDINGS.some((ending) => line.endsWith(ending))
      || (line.startsWith("<") && line.endsWith(">"))
      || CODE_LINE_PREFIXES.some((prefix) => line.startsWith(prefix))
      || CODE_OPERATORS.some((operator) => line.includes(operator));
  };
  const looksLikeSourceCode = (lines) => lines.split("\n").some(isCodeLine);
  const tag = (element) => element.localName?.toLowerCase() || "";
  const isCell = (element) => tag(element) === "td" || tag(element) === "th";
  const create = (name) => document.createElementNS(XHTML, name);

  const copyStyles = (source, target, includeBox) => {
    const style = getComputedStyle(source);
    for (const name of includeBox ? [...TEXT_STYLES, ...BOX_STYLES] : TEXT_STYLES) {
      const value = style.getPropertyValue(name);
      if (value) target.style.setProperty(name, value, "important");
    }
  };

  const clear = () => {
    for (const state of applied) {
      if (state.hidden) restoreOriginal(state);
      state.layer.remove();
      if (state.wrapper?.parentNode) state.wrapper.replaceWith(...state.wrapper.childNodes);
    }
    applied.length = 0;
    layerStates.clear();
  };

  // Only innermost blocks participate. Skipped subtrees do not contribute text
  // or prevent an otherwise innermost paragraph from being translated.
  const candidateElements = () => {
    const all = Array.from(document.body.querySelectorAll(SELECTOR))
      .filter((element) => !element.closest("script,style,noscript,template,pre,code"));
    const parents = new Set();
    for (const element of all) {
      const parent = element.parentElement?.closest(SELECTOR);
      if (parent) parents.add(parent);
    }
    return all.filter((element) => !parents.has(element));
  };

  const inspect = (element) => {
    const leaves = [];
    const text = [];
    // Only non-code leaves: a prose sentence that merely mentions `<code>x = 1;</code>`
    // is still prose, while a block whose own lines are statements is code. Line
    // breaks exist only as `<br>`, so they are marked in the shape text.
    const lines = [];
    const visit = (node, preserved) => {
      if (node.nodeType === Node.TEXT_NODE) {
        text.push(node.data);
        if (!preserved && translatable(node.data)) {
          leaves.push(node);
          lines.push(node.data);
        }
      } else if (node.nodeType === Node.ELEMENT_NODE && !SKIPPED.has(tag(node))) {
        if (!preserved && tag(node) === "br") lines.push("\n");
        for (const child of node.childNodes) visit(child, preserved || PRESERVED.has(tag(node)));
      }
    };
    visit(element, !!element.parentElement?.closest("pre,code"));
    return { source: normalize(text.join("")), leaves, lines: lines.join("") };
  };

  const validatedLeaves = (entry, source) => {
    if (!Array.isArray(entry.segments) || entry.segments.length !== source.leaves.length
      || !entry.segments.length) return null;
    const translated = new Map();
    for (let index = 0; index < source.leaves.length; index += 1) {
      const segment = entry.segments[index];
      const leaf = source.leaves[index];
      if (!segment || typeof segment.source !== "string"
        || normalize(segment.source) !== normalize(leaf.data)
        || typeof segment.translated !== "string" || !normalize(segment.translated)) return null;
      translated.set(leaf, segment.translated);
    }
    return translated;
  };

  const rebuild = (node, translated, pairs) => {
    if (node.nodeType === Node.TEXT_NODE) {
      // Keep boundary whitespace even if a provider trims a translated leaf.
      const value = translated.get(node);
      if (value === undefined) return document.createTextNode(node.data);
      const leading = node.data.match(/^\s*/u)[0];
      const trailing = node.data.match(/\s*$/u)[0];
      const copy = document.createTextNode(leading + value.trim() + trailing);
      // A translated leaf was built from exactly this original leaf; nothing can
      // map translated characters back one by one, so this is the finest anchor.
      pairs.push({ copy, origin: node });
      return copy;
    }
    if (node.nodeType !== Node.ELEMENT_NODE || SKIPPED.has(tag(node)) || node.matches(MEDIA)) return null;
    const copy = create(INLINE_TAGS.has(tag(node)) ? tag(node) : "span");
    copyStyles(node, copy, true);
    for (const child of node.childNodes) {
      const result = rebuild(child, translated, pairs);
      if (result) copy.append(result);
    }
    return copy;
  };

  const restoreOriginal = (state) => {
    const { original, display, priority, hadStyle } = state;
    if (display) original.style.setProperty("display", display, priority);
    else original.style.removeProperty("display");
    if (!hadStyle && !original.getAttribute("style")) original.removeAttribute("style");
    state.hidden = false;
  };

  const setCollapsed = (state, collapsed) => {
    if (collapsed) {
      state.original.style.setProperty("display", "none", "important");
      state.hidden = true;
    } else if (state.hidden) restoreOriginal(state);
    state.divider.style.display = collapsed ? "none" : "block";
    state.layer.setAttribute("data-moye-collapsed", collapsed ? "1" : "0");
  };

  const insert = (element, translated, translateOnly) => {
    const inPlace = isCell(element) || tag(element) === "li";
    const layer = create("div");
    layer.setAttribute(MARK, "1");
    layer.className = "moye-translation-block";
    layer.style.cssText = "display:block!important;margin:0!important;padding:0!important;";
    const text = create(inPlace ? "div" : tag(element));
    text.className = "moye-translation-text";
    copyStyles(element, text, !inPlace);
    text.style.setProperty("display", "block", "important");
    if (inPlace) {
      text.style.setProperty("margin", "0", "important");
      text.style.setProperty("padding", "0", "important");
      text.style.setProperty("border", "0", "important");
    }
    const pairs = [];
    for (const child of element.childNodes) {
      const result = rebuild(child, translated, pairs);
      if (result) text.append(result);
    }
    const divider = create("div");
    divider.className = "moye-translation-divider";
    divider.style.cssText = "display:block;border-top:1px dashed rgba(120,120,120,0.5);margin:0.35em 0;";
    layer.append(text, divider);

    let original = element;
    let wrapper = null;
    if (tag(element) === "li") {
      // Keep the real LI and its marker. Moving its existing nodes into a
      // reversible wrapper retains their identities and book-text order.
      wrapper = create("span");
      wrapper.style.setProperty("display", "contents", "important");
      wrapper.append(...element.childNodes);
      element.append(wrapper);
      original = wrapper;
    }
    const state = {
      original, wrapper, layer, divider, pairs, hidden: false,
      display: original.style.getPropertyValue("display"),
      priority: original.style.getPropertyPriority("display"),
      hadStyle: original.hasAttribute("style"),
    };
    layerStates.set(layer, state);
    const toggleable = !isCell(element) && !element.querySelector(MEDIA);
    const onToggle = (event) => {
      // Dragging to copy a translation must not collapse the paragraph.
      if (window.getSelection()?.toString()) return;
      event.preventDefault();
      event.stopPropagation();
      setCollapsed(state, !state.hidden);
    };
    if (toggleable) {
      text.style.setProperty("cursor", "pointer", "important");
      text.addEventListener("click", onToggle);
      layer.addEventListener("click", (event) => {
        if (event.target === layer) onToggle(event);
      });
    }
    if (inPlace) element.insertBefore(layer, element.firstChild);
    else element.parentNode.insertBefore(layer, element);
    setCollapsed(state, translateOnly && toggleable);
    applied.push(state);
  };

  const configure = (payload) => {
    if (!payload || typeof payload !== "object") return;
    session = typeof payload.session === "string" ? payload.session : "";
    clear();
    const queues = new Map();
    for (const entry of Array.isArray(payload.blocks) ? payload.blocks : []) {
      if (!entry || typeof entry.source !== "string") continue;
      const key = normalize(entry.source);
      if (!key) continue;
      if (!queues.has(key)) queues.set(key, []);
      queues.get(key).push(entry);
    }
    if (!queues.size || !document.body) return;
    for (const element of candidateElements()) {
      const source = inspect(element);
      // The host does not emit code-only blocks. They must not consume a
      // later translatable paragraph's entry when both have the same text.
      if (!source.leaves.length) continue;
      // Code is never translated, and the same rule must skip the same blocks on
      // both sides or a code block could consume a prose block's entry.
      if (looksLikeSourceCode(source.lines)) continue;
      const queue = queues.get(source.source);
      if (!queue?.length) continue;
      const translated = validatedLeaves(queue.shift(), source);
      // A stale/invalid result never hides book text. Consume its position so
      // repeated source blocks cannot silently borrow a later block's result.
      if (!translated || getComputedStyle(element).display === "none") continue;
      insert(element, translated, payload.displayMode === "translation-only");
    }
  };

  /// The original book-text range one selection on the applied translation layer
  /// stands for. Notes, marks and AI references always anchor to the immutable
  /// original, so such a selection is resolved here first. Translated characters
  /// cannot be mapped back one by one, so every translated leaf the selection
  /// touches contributes the whole original leaf it was built from, and the
  /// result spans from the first to the last of them.
  const originalRange = (range) => {
    if (!range || typeof range.intersectsNode !== "function") return null;
    const origins = [];
    for (const state of applied) {
      if (!range.intersectsNode(state.layer)) continue;
      const touched = state.pairs.filter((pair) => range.intersectsNode(pair.copy));
      if (!touched.length) {
        if (state.pairs.length) continue;
        origins.push(state.original);
        continue;
      }
      for (const pair of touched) origins.push(pair.origin);
    }
    if (!origins.length) return null;
    origins.sort((left, right) => {
      if (left === right) return 0;
      return left.compareDocumentPosition(right) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1;
    });
    const merged = document.createRange();
    merged.selectNodeContents(origins[0]);
    const last = origins[origins.length - 1];
    if (last !== origins[0]) {
      const tail = document.createRange();
      tail.selectNodeContents(last);
      merged.setEnd(tail.endContainer, tail.endOffset);
    }
    return merged;
  };

  window.moyeTranslations = Object.freeze({
    configure,
    clear: () => { session = ""; clear(); },
    applied: () => applied.length,
    session: () => session,
    originalRange,
  });
})();
