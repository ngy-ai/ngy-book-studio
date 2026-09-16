// Trusted reading-time translation layer. The model returns text leaves only;
// formatting always comes from the current book DOM, never from model markup.
//
// The reader may rewrite the model output of a single block by hand: the layer
// turns its translated leaves into plain-text editables, and the host persists
// the edit beside the machine text (see `translations.rs` / `db::translations`).
(() => {
  "use strict";

  const MARK = "data-ngy-translation";
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
  // The manual editor is presentation only. It never becomes book text, a note
  // anchor or a translation payload: those still read the original leaves.
  // Faded instead of `visibility:hidden`: opacity keeps the row in the tab order
  // so a keyboard-only reader can still reach 「编辑译文」, while
  // `pointer-events:none` keeps the invisible row unclickable. Its labels live in
  // a shadow root below `CONTROLS_STYLE`: chrome inside the layer would join the
  // block's text, so a copied translation would carry 「编辑译文」.
  const CONTROLS_STYLE = "display:block!important;margin:0 0 0.3em!important;padding:0!important;" +
    "font:0.78em/1.7 system-ui,\"Microsoft YaHei\",sans-serif!important;color:#8a8a8a!important;" +
    "letter-spacing:normal!important;text-align:right!important;" +
    "opacity:0!important;pointer-events:none!important;transition:opacity 0.12s ease-out;";
  const CONTROLS_ROW_STYLE = "display:block!important;margin:0!important;padding:0!important;" +
    "letter-spacing:normal!important;";
  const CONTROL_BUTTON_STYLE = "margin:0 0 0 0.5em!important;padding:0.1em 0.5em!important;" +
    "border:1px solid currentColor!important;border-radius:0.25em!important;background:transparent!important;" +
    "color:inherit!important;font:inherit!important;cursor:pointer!important;";
  const EDITABLE_STYLE = "outline:1px dashed rgba(90,130,200,0.75)!important;border-radius:2px!important;";
  // The reveal control trails the last translated character of a block. In
  // 「译文」mode the original is hidden, and this icon is the only way back to
  // it: clicking the translated text itself no longer toggles, because that is
  // where a reader selects and copies, and a drag used to flip the paragraph.
  // The icon is a `<button>` carrying an inline SVG and no text node at all, so
  // it can sit in the flow without joining the block's text: the layer's
  // `textContent` must stay exactly the translation it displays.
  const SVG_NAMESPACE = "http://www.w3.org/2000/svg";
  const REVEAL_STYLE = "display:inline-block!important;width:1em!important;height:1em!important;" +
    "margin:0 0 0 0.25em!important;padding:0!important;border:0!important;background:transparent!important;" +
    "color:inherit!important;opacity:0.45!important;cursor:pointer!important;vertical-align:-0.12em!important;" +
    "font:inherit!important;line-height:1!important;";
  const REVEAL_STROKE = "M2 12s3.6-6.5 10-6.5S22 12 22 12s-3.6 6.5-10 6.5S2 12 2 12z";
  // Shown while the original is hidden: an open eye reads as 「显示原文」.
  const REVEAL_SHOW = [REVEAL_STROKE, "M12 9.4a2.6 2.6 0 1 0 0 5.2 2.6 2.6 0 0 0 0-5.2z"];
  // Shown once the original is back: the same eye struck through reads as 「收起原文」.
  const REVEAL_HIDE = [...REVEAL_SHOW, "M3.5 3.5l17 17"];

  let session = "";
  let revision = 0;
  let requestId = 0;
  // A payload that arrived while the reader was editing the same chapter. The
  // background poll must not delete text being typed; it is applied as soon as
  // the editor closes.
  let deferred = null;
  const applied = [];
  // Applied layer element -> its state, so a selection made on a translation can
  // be resolved back to the original text it was built from.
  const layerStates = new Map();
  // In-flight manual translation requests, so a reply for a chapter that is gone
  // is simply dropped.
  const pending = new Map();
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
  // Statement keywords only count together with a code shape on the same line:
  // prose opens sentences with `let` / `use` / `return` too.
  const CODE_LINE_KEYWORDS = [
    "def", "fn", "func", "impl", "struct", "enum", "trait", "class", "let", "var", "const",
    "static", "public", "private", "protected", "pub", "use", "import", "package", "export",
    "return", "println", "printf", "console", "system",
  ];
  // A bare quote is never a signal: prose wraps quoted words in spaces, so a quote
  // on its own would keep every paragraph with a quotation in the original language.
  const CODE_STRING_SHAPES = ["+\"", "\"+", "+ \"", "\" +", "(\"", "\")", "('", "')"];
  const trimCodeLine = (value) =>
    value.replace(/^[\s\u200b\ufeff\u00ad]+|[\s\u200b\ufeff\u00ad]+$/gu, "");
  const hasCodeStringShape = (line) =>
    CODE_STRING_SHAPES.some((shape) => line.includes(shape)) || /\\[ntrbf0\\'"]/.test(line);
  const hasCodeKeyword = (line) => {
    const word = /^[A-Za-z0-9_.]+/.exec(line)?.[0];
    if (!word || !CODE_LINE_KEYWORDS.includes(word)) return false;
    const rest = line.slice(word.length);
    return rest.includes("(") || rest.includes("=") || rest.includes("{") || rest.includes("[");
  };
  // An unspaced type annotation (`a:Int`). Prose writes `Note: the file`, and a URL
  // has a slash after its colon.
  const hasTypeAnnotation = (line) => /:[A-Za-z]/.test(line);
  const isCodeLine = (value) => {
    const line = trimCodeLine(value);
    if (!line) return false;
    return CODE_LINE_ENDINGS.some((ending) => line.endsWith(ending))
      || (line.startsWith("<") && line.endsWith(">"))
      || CODE_LINE_PREFIXES.some((prefix) => line.startsWith(prefix))
      || CODE_OPERATORS.some((operator) => line.includes(operator))
      || hasCodeStringShape(line)
      || hasCodeKeyword(line)
      || hasTypeAnnotation(line);
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
    pending.clear();
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
      pairs.push({ copy, origin: node, value: value.trim(), leading, trailing, node: null });
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

  /// Points the trailing icon at the state of its block: an eye while the
  /// original is hidden, a struck-through eye once it is visible again. The
  /// label lives on the button as an attribute; SVG `<title>` or a glyph would
  /// be text inside the layer and would be copied with the translation.
  const renderReveal = (state) => {
    const reveal = state.reveal;
    if (!reveal) return;
    reveal.show.style.setProperty("display", state.hidden ? "block" : "none", "important");
    reveal.hide.style.setProperty("display", state.hidden ? "none" : "block", "important");
    const label = state.hidden ? "显示原文" : "收起原文";
    reveal.element.setAttribute("aria-label", label);
    reveal.element.setAttribute("title", label);
  };

  const setCollapsed = (state, collapsed) => {
    if (collapsed) {
      state.original.style.setProperty("display", "none", "important");
      state.hidden = true;
    } else if (state.hidden) restoreOriginal(state);
    state.divider.style.display = collapsed ? "none" : "block";
    state.layer.setAttribute("data-ngy-collapsed", collapsed ? "1" : "0");
    renderReveal(state);
  };

  const button = (label, handler) => {
    const element = create("button");
    element.type = "button";
    element.textContent = label;
    element.style.cssText = CONTROL_BUTTON_STYLE;
    element.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();
      handler();
    });
    return element;
  };

  /// What the editor offers for one applied block: nothing while a save is in
  /// flight, save/cancel while editing, and edit plus the manual state otherwise.
  const renderControls = (state) => {
    const controls = state.controls;
    if (!controls) return;
    controls.textContent = "";
    if (state.error) {
      const error = create("span");
      error.className = "ngy-translation-error";
      error.textContent = state.error;
      error.style.setProperty("color", "#c0392b", "important");
      error.style.setProperty("margin-right", "0.5em", "important");
      controls.append(error);
    } else if (state.manual) {
      const marker = create("span");
      marker.className = "ngy-translation-manual";
      marker.textContent = "已手工修改";
      marker.style.setProperty("margin-right", "0.5em", "important");
      controls.append(marker);
    }
    if (state.request) {
      const busy = create("span");
      busy.textContent = "保存中…";
      controls.append(busy);
    } else if (state.editing) {
      controls.append(
        button("保存", () => saveEdit(state)),
        button("取消", () => endEdit(state, true)),
      );
    } else {
      controls.append(button("编辑译文", () => beginEdit(state)));
      if (state.manual) {
        controls.append(button("恢复机器译文", () => {
          state.error = "";
          submit(state, "restore", []);
        }));
      }
    }
    // Editing, saving and failures pin the row open; otherwise the pointer decides.
    state.showControls?.(state.hovering);
  };

  /// Turns the translated leaves of one block into plain-text editables. The
  /// original book text must not change: failure or cancel puts the exact text
  /// node back, and a save is revalidated by the host against the stored row.
  const beginEdit = (state) => {
    if (state.editing || state.request || !state.key) return;
    state.editing = true;
    state.error = "";
    for (const pair of state.pairs) {
      const editable = create("span");
      editable.className = "ngy-translation-editable";
      editable.setAttribute("contenteditable", "true");
      editable.style.cssText = EDITABLE_STYLE;
      editable.textContent = pair.value;
      editable.onkeydown = (event) => {
        // A leaf is one line: a paragraph break inside one could not be stored
        // or rendered. Escape abandons the edit.
        if (event.key === "Enter") event.preventDefault();
        else if (event.key === "Escape") { event.preventDefault(); endEdit(state, true); }
      };
      editable.onpaste = (event) => {
        event.preventDefault();
        insertPlainText(event.clipboardData?.getData("text/plain") ?? "");
      };
      pair.node = pair.copy;
      pair.copy = editable;
      pair.node.replaceWith(editable);
    }
    renderControls(state);
    const first = state.pairs[0]?.copy;
    if (first) focusEnd(first);
  };

  const insertPlainText = (text) => {
    const selection = window.getSelection();
    if (!selection || selection.rangeCount !== 1 || !text) return;
    const range = selection.getRangeAt(0);
    range.deleteContents();
    const node = document.createTextNode(text);
    range.insertNode(node);
    range.setStartAfter(node);
    range.collapse(true);
    selection.removeAllRanges();
    selection.addRange(range);
  };

  const focusEnd = (element) => {
    element.focus({ preventScroll: true });
    const range = document.createRange();
    range.selectNodeContents(element);
    range.collapse(false);
    const selection = window.getSelection();
    if (!selection) return;
    selection.removeAllRanges();
    selection.addRange(range);
  };

  /// Leaves edit mode. `revert` restores the pre-edit text; otherwise the typed
  /// text stays until the host republishes the chapter from the stored row.
  const endEdit = (state, revert) => {
    for (const pair of state.pairs) {
      if (!pair.node) continue;
      if (revert) {
        pair.copy.replaceWith(pair.node);
        pair.copy = pair.node;
      } else {
        const settled = document.createTextNode(
          pair.leading + pair.copy.textContent.trim() + pair.trailing,
        );
        pair.copy.replaceWith(settled);
        pair.copy = settled;
      }
      pair.node = null;
    }
    state.editing = false;
    renderControls(state);
    flushDeferred();
  };

  const saveEdit = (state) => {
    const segments = state.pairs.map((pair, index) => ({
      // The host must find this string in the stored row, so prefer the source it
      // pushed: a leaf's own text can differ in whitespace alone.
      source: state.sources?.[index] ?? pair.origin.data,
      translated: pair.copy.textContent.trim(),
    }));
    if (segments.some((segment) => !segment.translated)) {
      state.error = "译文不能为空；如需还原请使用「恢复机器译文」";
      renderControls(state);
      return;
    }
    state.error = "";
    submit(state, "update", segments);
  };

  const submit = (state, action, segments) => {
    if (state.request || !state.key) return;
    if (!window.ipc?.postMessage) {
      state.error = "无法连接阅读窗口，译文未能保存";
      renderControls(state);
      return;
    }
    requestId += 1;
    state.request = requestId;
    state.requestManual = action === "update";
    pending.set(requestId, state);
    try {
      window.ipc.postMessage(JSON.stringify({
        type: "manual_translation",
        action,
        revision,
        request_id: state.request,
        key: state.key,
        segments,
      }));
    } catch {
      pending.delete(state.request);
      state.request = 0;
      state.error = "无法发送译文修改请求";
    }
    renderControls(state);
  };

  /// The host's answer to one manual translation request. A request id this page
  /// never sent (an answer for a chapter that is already gone) is ignored.
  const applyResult = (payload) => {
    if (!payload || typeof payload !== "object") return;
    const state = pending.get(payload.request_id);
    if (!state || state.request !== payload.request_id) return;
    pending.delete(payload.request_id);
    state.request = 0;
    if (payload.ok) {
      state.manual = state.requestManual === true;
      state.error = "";
      endEdit(state, false);
      return;
    }
    state.error = typeof payload.error === "string" && payload.error
      ? payload.error
      : "译文未能保存";
    renderControls(state);
  };

  const svgIcon = (paths) => {
    const svg = document.createElementNS(SVG_NAMESPACE, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    svg.setAttribute("style", "display:block;width:1em;height:1em;");
    for (const d of paths) {
      const path = document.createElementNS(SVG_NAMESPACE, "path");
      path.setAttribute("d", d);
      svg.append(path);
    }
    return svg;
  };

  /// Appends the reveal icon to the end of one block's translated text. The
  /// block is addressed through the state it was built with, so the icon only
  /// ever collapses the paragraph it belongs to.
  const appendReveal = (state, text) => {
    const element = create("button");
    element.type = "button";
    element.className = "ngy-translation-reveal";
    element.setAttribute("data-ngy-translation-reveal", "1");
    element.style.cssText = REVEAL_STYLE;
    const show = svgIcon(REVEAL_SHOW);
    const hide = svgIcon(REVEAL_HIDE);
    element.append(show, hide);
    element.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();
      setCollapsed(state, !state.hidden);
    });
    state.reveal = { element, show, hide };
    text.append(element);
    renderReveal(state);
  };

  const insert = (element, entry, translated, translateOnly) => {
    const inPlace = isCell(element) || tag(element) === "li";
    const layer = create("div");
    layer.setAttribute(MARK, "1");
    layer.className = "ngy-translation-block";
    layer.style.cssText = "display:block!important;margin:0!important;padding:0!important;";
    const text = create(inPlace ? "div" : tag(element));
    text.className = "ngy-translation-text";
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
    divider.className = "ngy-translation-divider";
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
      // The stored source of each leaf, in the same order as `pairs`. The host
      // revalidates a manual edit against these exact strings.
      sources: Array.isArray(entry.segments)
        ? entry.segments.map((segment) => segment.source)
        : [],
      key: typeof entry.key === "string" ? entry.key : "",
      manual: entry.manual === true,
      editing: false, request: 0, requestManual: false, error: "", hovering: false,
      controls: null, showControls: null, reveal: null,
      display: original.style.getPropertyValue("display"),
      priority: original.style.getPropertyPriority("display"),
      hadStyle: original.hasAttribute("style"),
    };
    layerStates.set(layer, state);
    // 「译文」is the only mode that hides the original, so the reveal icon only
    // exists there, and a block that may not be collapsed at all — a table cell
    // or a paragraph carrying media — stays bilingual without one. Decided
    // before any chrome is inserted: the icon is a `<button>` and `MEDIA`
    // matches buttons, so a later check would disable itself.
    const revealable = translateOnly && !isCell(element) && !element.querySelector(MEDIA);
    if (inPlace) element.insertBefore(layer, element.firstChild);
    else element.parentNode.insertBefore(layer, element);
    if (revealable) appendReveal(state, text);
    appendControls(state);
    setCollapsed(state, revealable);
    applied.push(state);
  };

  /// Adds the manual-edit row to one applied block. It sits between the
  /// translation and the divider, appears on hover, and is inert for a block the
  /// page cannot address (no key means the host cannot find the stored row).
  ///
  /// The row's contents live in a shadow root: labels are chrome, and light DOM
  /// text inside the layer would be copied with a selection and would make the
  /// layer report more text than the translation it displays. The host element
  /// (the shadow boundary) carries the visibility state instead.
  const appendControls = (state) => {
    if (!state.key) return;
    const host = create("span");
    host.className = "ngy-translation-controls";
    host.setAttribute("data-ngy-translation-controls", "1");
    host.style.cssText = CONTROLS_STYLE;
    const row = create("span");
    row.className = "ngy-translation-row";
    row.style.cssText = CONTROLS_ROW_STYLE;
    host.attachShadow({ mode: "open" }).append(row);
    state.controls = row;
    state.showControls = (visible) => {
      const pinned = state.editing || state.request > 0 || !!state.error;
      const shown = visible || pinned;
      host.style.setProperty("opacity", shown ? "1" : "0", "important");
      host.style.setProperty("pointer-events", shown ? "auto" : "none", "important");
    };
    state.layer.addEventListener("mouseenter", () => {
      state.hovering = true;
      state.showControls(true);
    });
    state.layer.addEventListener("mouseleave", () => {
      state.hovering = false;
      state.showControls(false);
    });
    // Tabbing into the hidden row reveals it, so keyboard users can see where
    // focus is before pressing Enter. Focus events cross the shadow boundary.
    host.addEventListener("focusin", () => state.showControls(true));
    host.addEventListener("focusout", () => state.showControls(state.hovering));
    state.layer.insertBefore(host, state.divider);
    renderControls(state);
  };

  const applyPayload = (payload) => {
    session = typeof payload.session === "string" ? payload.session : "";
    revision = Number.isSafeInteger(payload.revision) ? payload.revision : 0;
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
      const entry = queue.shift();
      const translated = validatedLeaves(entry, source);
      // A stale/invalid result never hides book text. Consume its position so
      // repeated source blocks cannot silently borrow a later block's result.
      if (!translated || getComputedStyle(element).display === "none") continue;
      insert(element, entry, translated, payload.displayMode === "translation-only");
    }
  };

  const editing = () => applied.some((state) => state.editing || state.request > 0);

  const flushDeferred = () => {
    if (!deferred || editing()) return;
    const payload = deferred;
    deferred = null;
    applyPayload(payload);
  };

  const configure = (payload) => {
    if (!payload || typeof payload !== "object") return;
    // The background poll republishes the chapter while its translations advance.
    // That must never delete an edit in progress: the newest payload for the same
    // chapter waits until the editor closes. A payload for another chapter is a
    // navigation and applies immediately.
    const next = Number.isSafeInteger(payload.revision) ? payload.revision : 0;
    if (editing() && next === revision) {
      deferred = payload;
      return;
    }
    deferred = null;
    applyPayload(payload);
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

  window.ngyTranslations = Object.freeze({
    configure,
    result: applyResult,
    clear: () => { session = ""; deferred = null; clear(); },
    applied: () => applied.length,
    session: () => session,
    originalRange,
  });
})();
