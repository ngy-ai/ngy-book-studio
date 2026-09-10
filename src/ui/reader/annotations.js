(() => {
  "use strict";

  // This bridge is injected by the host. The chapter's scripts remain disabled.
  const ownDocument = () => {
    const url = new URL(window.location.href);
    return window.top === window && (
      (url.protocol === "epubreader:" && url.hostname === "book") ||
      ((url.protocol === "http:" || url.protocol === "https:") &&
        url.hostname === "epubreader.book")
    );
  };
  if (!ownDocument()) return;

  const encoder = new TextEncoder();
  const MAX_AI_BYTES = 1024 * 1024;
  const MAX_CONTENT_HTML_BYTES = 8 * MAX_AI_BYTES;
  const HTML_NS = "http://www.w3.org/1999/xhtml";
  const markdownTags = new Set([
    "p", "h1", "h2", "h3", "h4", "h5", "h6", "strong", "em", "b", "i", "del", "s",
    "ul", "ol", "li", "blockquote", "pre", "code", "table", "thead", "tbody", "tfoot",
    "tr", "th", "td", "hr", "br",
  ]);
  const excludedTags = new Set([
    "script", "style", "template", "noscript", "iframe", "object", "embed", "img",
    "svg", "math", "video", "audio", "source", "track", "link", "meta", "base",
  ]);
  const compact = (value) => value.replace(/\s/gu, "");
  const normalize = (value) => value.replace(/\s+/gu, " ").trim();
  const bounded = (value, bytes) => typeof value === "string" &&
    encoder.encode(value).byteLength <= bytes;
  const kinds = {
    highlight: "马克笔", wavy: "波浪线", underline: "直线",
    human_comment: "人工想法", ai_comment: "AI 想法",
  };
  let context = null;
  let ready = false;
  let requestSequence = 0;
  let notes = [];
  let noteScope = null;
  let selectionSnapshot = null;
  let contextMenuSnapshot = null;
  let draftDirty = false;
  let failedAiSave = null;
  let editor = null;
  let index = null;
  let paintFrame = 0;
  let selectionTimer = 0;
  let hitTimer = 0;
  let toastTimer = 0;
  let host, root, toolbar, layer, drawer, drawerTitle, list, editorPanel, quoteView;
  let input, saveButton, editorTitle, editorStatus, toggle, toast, listStatus;
  const pending = new Map();
  let hitAreas = [];

  function element(tag, className, text) {
    // Reader chapters may be application/xhtml+xml; createElement alone would
    // create namespace-less XML nodes without HTML controls or a shadow host.
    const node = document.createElementNS(HTML_NS, tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function button(label, className, action) {
    const node = element("button", className, label);
    node.type = "button";
    node.addEventListener("click", action);
    return node;
  }

  function thoughtContent(thought) {
    const fallback = () => element("p", "content", thought.content || "");
    if (!bounded(thought.content_html, MAX_CONTENT_HTML_BYTES)) return fallback();
    try {
      // An HTML owner document gives template the HTML fragment parser even
      // when the reading chapter is XHTML. Template content stays inert; no
      // supplied node or attribute is ever attached to the live document.
      const inert = document.implementation.createHTMLDocument("");
      const template = inert.createElementNS(HTML_NS, "template");
      template.innerHTML = thought.content_html;
      const view = element("div", "content markdown");
      const pendingNodes = [...template.content.childNodes].reverse()
        .map((source) => ({ source, parent: view, depth: 0 }));
      let count = 0;
      while (pendingNodes.length) {
        const { source, parent, depth } = pendingNodes.pop();
        if (++count > 20000 || depth > 64) return fallback();
        if (source.nodeType === Node.TEXT_NODE) {
          parent.append(document.createTextNode(source.textContent));
          continue;
        }
        if (source.nodeType !== Node.ELEMENT_NODE || source.namespaceURI !== HTML_NS) continue;
        const tag = source.localName;
        if (excludedTags.has(tag)) continue;
        if (tag === "input") {
          if (source.getAttribute("type") === "checkbox") {
            const checked = source.hasAttribute("checked");
            const checkbox = element("span", "task-checkbox", checked ? "☑" : "☐");
            checkbox.setAttribute("aria-label", checked ? "已完成" : "未完成");
            parent.append(checkbox);
          }
          continue;
        }
        let destination = parent;
        if (markdownTags.has(tag)) {
          destination = element(tag);
          if (tag === "ol" && /^\d{1,9}$/.test(source.getAttribute("start") || "")) {
            destination.setAttribute("start", source.getAttribute("start"));
          }
          if (tag === "th" || tag === "td") {
            const align = source.getAttribute("align");
            if (["left", "center", "right"].includes(align)) destination.style.textAlign = align;
          }
          parent.append(destination);
        }
        for (const child of [...source.childNodes].reverse()) {
          pendingNodes.push({ source: child, parent: destination, depth: depth + 1 });
        }
      }
      return view;
    } catch {
      return fallback();
    }
  }

  function copyThought(content) {
    const field = element("textarea", "copy-source");
    field.value = content;
    field.readOnly = true;
    const focused = root.activeElement;
    const writeSource = (event) => {
      if (!event.clipboardData) return;
      event.clipboardData.setData("text/plain", content);
      event.preventDefault();
    };
    root.append(field);
    field.focus({ preventScroll: true });
    field.select();
    document.addEventListener("copy", writeSource, true);
    try {
      const copied = document.execCommand("copy");
      tell(copied ? "已复制想法原文。" : "复制失败，请选择想法文字后按 Ctrl + C。", !copied);
    } catch {
      tell("复制失败，请选择想法文字后按 Ctrl + C。", true);
    } finally {
      document.removeEventListener("copy", writeSource, true);
      field.remove();
      focused?.focus({ preventScroll: true });
    }
  }

  function matches(payload) {
    return context && payload && payload.session === context.session &&
      payload.revision === context.revision;
  }

  function tell(message, error = false) {
    if (!ready) return;
    toast.textContent = message;
    toast.classList.toggle("error", error);
    toast.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toast.hidden = true; }, error ? 7000 : 3000);
  }

  function post(action, fields = {}) {
    if (!context || !ready || !ownDocument()) return null;
    const requestId = ++requestSequence;
    if (!Number.isSafeInteger(requestId)) return null;
    pending.set(requestId, { action, ...fields });
    try {
      if (!window.ipc || typeof window.ipc.postMessage !== "function") {
        throw new Error("Reader host unavailable");
      }
      window.ipc.postMessage(JSON.stringify({
        type: "annotation_action", action, session: context.session,
        revision: context.revision, request_id: requestId, ...fields,
      }));
      return requestId;
    } catch {
      pending.delete(requestId);
      tell("无法连接笔记服务，请重新打开本章后重试。", true);
      return null;
    }
  }

  function reportDraft(dirty) {
    if (draftDirty === dirty) return;
    draftDirty = dirty;
    const requestId = post("draft_changed", { dirty });
    // This notification has no persistence operation or asynchronous result.
    if (requestId !== null) pending.delete(requestId);
  }

  // Offsets count UTF-16 code units after ECMAScript whitespace is removed.
  // Keep original text nodes intact: selections, links, and copy use book text.
  function textIndex() {
    if (index) return index;
    const entries = [];
    const parts = [];
    let length = 0;
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        return node.parentElement?.closest("script,style,noscript,template") ||
          host?.contains(node) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT;
      },
    });
    while (walker.nextNode()) {
      const node = walker.currentNode;
      const text = compact(node.data);
      if (!text) continue;
      entries.push({ node, start: length, end: length + text.length });
      parts.push(text);
      length += text.length;
    }
    index = { entries, text: parts.join("") };
    return index;
  }

  function currentSelection() {
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || selection.rangeCount !== 1) return null;
    const range = selection.getRangeAt(0);
    if (!document.body.contains(range.startContainer) ||
        !document.body.contains(range.endContainer)) return null;
    const quote = normalize(selection.toString());
    if (!quote || !bounded(quote, 32768)) return null;
    let start = null;
    let end = null;
    const current = textIndex();
    for (const entry of current.entries) {
      if (!range.intersectsNode(entry.node)) continue;
      const from = range.startContainer === entry.node ? range.startOffset : 0;
      const to = range.endContainer === entry.node ? range.endOffset : entry.node.length;
      if (!compact(entry.node.data.slice(from, to))) continue;
      const nodeStart = entry.start + compact(entry.node.data.slice(0, from)).length;
      const nodeEnd = entry.start + compact(entry.node.data.slice(0, to)).length;
      if (start === null) start = nodeStart;
      end = nodeEnd;
    }
    if (start === null || end === null || end <= start ||
        current.text.slice(start, end) !== compact(quote)) return null;
    return { anchor: { quote, start, end }, range: range.cloneRange() };
  }

  function rawOffset(text, offset, after) {
    let count = 0;
    for (let position = 0; position < text.length; position++) {
      if (/\s/u.test(text[position])) continue;
      if (!after && count === offset) return position;
      count++;
      if (after && count === offset) return position + 1;
    }
    return text.length;
  }

  function anchorRange(anchor) {
    if (!anchor || !bounded(anchor.quote, 32768) ||
        !Number.isSafeInteger(anchor.start) || !Number.isSafeInteger(anchor.end) ||
        anchor.start < 0 || anchor.end <= anchor.start) return null;
    const current = textIndex();
    if (current.text.slice(anchor.start, anchor.end) !== compact(anchor.quote)) return null;
    const first = current.entries.find((entry) =>
      entry.start <= anchor.start && entry.end > anchor.start);
    const last = current.entries.find((entry) =>
      entry.start < anchor.end && entry.end >= anchor.end);
    if (!first || !last) return null;
    const range = document.createRange();
    range.setStart(first.node, rawOffset(first.node.data, anchor.start - first.start, false));
    range.setEnd(last.node, rawOffset(last.node.data, anchor.end - last.start, true));
    return range;
  }

  function showSelection() {
    if (!ready || !context || (editor && !drawer.hidden)) return;
    const selected = currentSelection();
    if (!selected) {
      toolbar.hidden = true;
      selectionSnapshot = null;
      return;
    }
    selectionSnapshot = selected;
    toolbar.hidden = false;
    positionToolbar();
  }

  function positionToolbar() {
    if (!selectionSnapshot || toolbar.hidden) return;
    const rects = Array.from(selectionSnapshot.range.getClientRects());
    const rect = rects.find((item) => item.bottom > 0 && item.top < innerHeight);
    if (!rect) { toolbar.hidden = true; return; }
    const viewportWidth = document.documentElement.clientWidth || innerWidth;
    toolbar.style.maxWidth = `${Math.max(1, viewportWidth - 16)}px`;
    const width = toolbar.offsetWidth;
    const height = toolbar.offsetHeight;
    const left = Math.max(8, Math.min(viewportWidth - width - 8, rect.left + rect.width / 2 - width / 2));
    let top = rect.top - height - 12;
    if (top < 8) top = Math.min(innerHeight - height - 8, rect.bottom + 12);
    toolbar.style.left = `${left}px`;
    toolbar.style.top = `${Math.max(8, top)}px`;
  }

  function choose(action) {
    if (!selectionSnapshot) return;
    const anchor = selectionSnapshot.anchor;
    if (!anchorRange(anchor)) { tell("选中文字已变化，请重新选择。", true); return; }
    if (action === "human_comment") {
      openEditor({ anchor, value: "" });
    } else {
      const requestId = post(action, { anchor });
      if (requestId !== null) {
        if (action === "ai_explain") {
          openDrawer(null);
          tell("AI 正在解释选中文字，完成后会保存为 AI 想法。");
        } else tell(action === "remove_mark" ? "正在删除划线…" : "正在保存笔记…");
      }
    }
    toolbar.hidden = true;
  }

  // Two anchors describe the same selection only when the compacted range and
  // the quote agree; equal offsets over different text never merge.
  function sameAnchor(left, right) {
    return !!left && !!right && left.start === right.start && left.end === right.end &&
      compact(left.quote) === compact(right.quote);
  }

  function inScope(note, scope) {
    return scope === null || (!note.stale && note.anchor &&
      scope.some((anchor) => sameAnchor(anchor, note.anchor)));
  }

  function inNoteScope(note) {
    return inScope(note, noteScope);
  }

  // The related-notes drawer lists thoughts only. An AI thought that is still
  // generating or waiting to be saved also holds a card, so it counts here and
  // the drawer opens instead of falling back to a book selection.
  function hasRelatedThought(scope) {
    if (failedAiSave && inScope(failedAiSave, scope)) return true;
    for (const request of pending.values()) {
      if (request.action === "ai_explain" && inScope(request, scope)) return true;
    }
    return notes.some((note) => (note.kind === "human_comment" || note.kind === "ai_comment") &&
      inScope(note, scope));
  }

  // Clicking a mark that carries no thought selects exactly the marked range so
  // the shared toolbar can restyle or delete the mark, or start a thought on it.
  function selectMarkedRange(noteId) {
    const note = notes.find((candidate) => candidate.id === noteId);
    const range = note && !note.stale ? anchorRange(note.anchor) : null;
    if (!range) return;
    const selection = window.getSelection();
    if (!selection) return;
    selection.removeAllRanges();
    selection.addRange(range);
    showSelection();
  }

  function openDrawer(scope = noteScope) {
    noteScope = scope;
    drawer.hidden = false;
    toggle.setAttribute("aria-expanded", "true");
    refreshList();
  }

  function closeDrawer() {
    drawer.hidden = true;
    toggle.setAttribute("aria-expanded", "false");
    // A hidden drawer keeps unsaved thoughts until explicit cancel or save.
    toggle.focus({ preventScroll: true });
  }

  function openEditor(draft) {
    if (editor && (editor.pending || editor.value.trim())) {
      openDrawer(null);
      tell("请先保存或取消当前想法，再添加另一条。", true);
      input.focus({ preventScroll: true });
      return;
    }
    editor = draft;
    reportDraft(true);
    editorPanel.hidden = false;
    editorTitle.textContent = draft.id ? "编辑人工想法" : "写想法 · 人工想法";
    quoteView.textContent = draft.anchor.quote;
    input.value = draft.value;
    input.disabled = false;
    saveButton.disabled = false;
    editorStatus.textContent = "仅保存你输入的内容。Ctrl + Enter 保存。";
    openDrawer(draft.id ? noteScope : null);
    input.focus({ preventScroll: true });
  }

  function saveEditor() {
    if (!editor || editor.pending) return;
    const value = input.value.trim();
    editor.value = input.value;
    if (!value) { editorStatus.textContent = "请输入你的想法。"; input.focus(); return; }
    if (!bounded(value, 65536)) {
      editorStatus.textContent = "想法过长，请缩短至 64 KiB 以内后重试。";
      return;
    }
    const fields = editor.id ? { id: editor.id, content: value } :
      { anchor: editor.anchor, content: value };
    const requestId = post(editor.id ? "update" : "human_comment", fields);
    if (requestId === null) return;
    editor.pending = requestId;
    input.disabled = true;
    saveButton.disabled = true;
    editorStatus.textContent = "正在保存…";
  }

  function refreshList() {
    if (!ready) return;
    toggle.textContent = `笔记 ${notes.length}`;
    drawerTitle.textContent = noteScope === null ? "本章笔记" : "划线相关笔记";
    drawer.setAttribute("aria-label", drawerTitle.textContent);
    editorPanel.hidden = !editor || !inNoteScope(editor);
    const visibleNotes = notes.filter((note) => inNoteScope(note) && (noteScope === null ||
      note.kind === "human_comment" || note.kind === "ai_comment"));
    list.replaceChildren();
    let aiCount = 0;
    if (failedAiSave && inNoteScope(failedAiSave)) {
      aiCount++;
      const card = element("article", "note pending-note");
      card.append(element("div", "note-type", "AI 想法 · 尚未保存"));
      card.append(element("blockquote", "quote", failedAiSave.anchor?.quote || ""));
      card.append(thoughtContent(failedAiSave));
      card.append(element("p", "editor-status", failedAiSave.error));
      const retry = button("重试保存", "save", () => {
        const requestId = post("retry_ai_save");
        if (requestId !== null) {
          failedAiSave.pending = requestId;
          failedAiSave.pendingAction = "retry";
          refreshList();
        }
      });
      retry.disabled = !!failedAiSave.pending;
      if (failedAiSave.pendingAction === "retry") retry.textContent = "正在保存…";
      const discard = button("放弃", "text-button danger", () => {
        const requestId = post("discard_ai_save");
        if (requestId !== null) {
          failedAiSave.pending = requestId;
          failedAiSave.pendingAction = "discard";
          refreshList();
        }
      });
      discard.disabled = !!failedAiSave.pending;
      if (failedAiSave.pendingAction === "discard") discard.textContent = "正在放弃…";
      const actions = element("div", "note-actions");
      actions.append(button("复制想法", "text-button copy-thought", () => copyThought(failedAiSave.content)),
        discard, retry);
      // Keep recovery controls reachable even for a very long AI response.
      card.insertBefore(actions, card.querySelector(".content"));
      list.append(card);
    }
    for (const request of pending.values()) {
      if (request.action !== "ai_explain" || !inNoteScope(request)) continue;
      aiCount++;
      const card = element("article", "note pending-note");
      card.append(element("div", "note-type", "AI 想法 · 生成中"));
      card.append(element("blockquote", "quote", request.anchor?.quote || ""));
      card.append(thoughtContent({ ...request, content: request.content || "正在解释选中文字…" }));
      list.append(card);
    }
    listStatus.textContent = visibleNotes.length || aiCount ? "" : noteScope === null ?
      "本章还没有笔记。选择正文可以标记或写想法。" : "这处划线还没有相关想法。";
    for (const note of visibleNotes) {
      const card = element("article", `note ${note.kind}`);
      card.dataset.noteId = note.id;
      const header = element("div", "note-header");
      header.append(element("span", "note-type", kinds[note.kind]));
      const range = note.stale ? null : anchorRange(note.anchor);
      if (!range) header.append(element("span", "stale", "原文已变化"));
      card.append(header, element("blockquote", "quote", note.anchor.quote));
      if (note.content) card.append(thoughtContent(note));
      const actions = element("div", "note-actions");
      if (range) actions.append(button("定位原文", "text-button", () => {
        const target = anchorRange(note.anchor);
        if (!target) return;
        const rect = target.getBoundingClientRect();
        window.scrollBy({ top: rect.top - innerHeight / 3, behavior: "smooth" });
        tell(`已定位${kinds[note.kind]}。`);
      }));
      if (note.kind === "human_comment") actions.append(button("编辑", "text-button", () => {
        openEditor({ id: note.id, anchor: note.anchor, value: note.content || "" });
      }));
      if (note.content) actions.append(button("复制想法", "text-button copy-thought", () => copyThought(note.content)));
      const remove = button("删除", "text-button danger", () => {
        if (editor?.id === note.id && editor.pending) return;
        const requestId = post("delete", { id: note.id });
        if (requestId !== null) { remove.disabled = true; tell("正在删除笔记…"); }
      });
      remove.disabled = Array.from(pending.values()).some((request) =>
        request.id === note.id && request.action === "delete");
      actions.append(remove);
      card.append(actions);
      list.append(card);
    }
  }

  function paint() {
    paintFrame = 0;
    if (!ready) return;
    const fragment = document.createDocumentFragment();
    hitAreas = [];
    for (const note of notes) {
      if (note.stale) continue;
      const range = anchorRange(note.anchor);
      if (!range) continue;
      const seen = new Set();
      for (const rect of Array.from(range.getClientRects()).slice(0, 2048)) {
        if (rect.width < 0.5 || rect.height < 0.5 || rect.bottom < 0 ||
            rect.top > innerHeight || rect.right < 0 || rect.left > innerWidth) continue;
        const key = `${rect.x},${rect.y},${rect.width},${rect.height}`;
        if (seen.has(key)) continue;
        seen.add(key);
        const mark = element("span", `mark ${note.kind}`);
        mark.style.left = `${rect.left}px`;
        mark.style.top = `${rect.top}px`;
        mark.style.width = `${rect.width}px`;
        mark.style.height = `${rect.height}px`;
        fragment.append(mark);
        hitAreas.push({ id: note.id, rect });
      }
    }
    layer.replaceChildren(fragment);
    positionToolbar();
  }

  function schedulePaint() {
    if (!paintFrame) paintFrame = requestAnimationFrame(paint);
  }

  function receiveNotes(value) {
    if (!Array.isArray(value)) return;
    notes = value.filter((note) => note && typeof note.id === "string" &&
      Object.hasOwn(kinds, note.kind) && note.anchor && bounded(note.anchor.quote, 32768) &&
      (note.content == null || bounded(note.content, note.kind === "ai_comment" ? MAX_AI_BYTES : 65536)));
    if (ready) { refreshList(); schedulePaint(); }
  }

  const api = Object.freeze({
    configure(payload) {
      if (!payload || typeof payload.session !== "string" || !payload.session ||
          !Number.isSafeInteger(payload.revision) || payload.revision < 0) return false;
      const changed = !matches(payload);
      context = { session: payload.session, revision: payload.revision };
      if (changed) {
        pending.clear();
        notes = [];
        noteScope = null;
        selectionSnapshot = null;
        contextMenuSnapshot = null;
        draftDirty = false;
        failedAiSave = null;
        editor = null;
        if (ready) {
          editorPanel.hidden = true;
          toolbar.hidden = true;
          input.value = "";
        }
      }
      receiveNotes(payload.notes || []);
      if (ready && changed) post("list");
      return true;
    },
    render(payload) {
      if (!matches(payload)) return false;
      receiveNotes(payload.notes);
      return true;
    },
    result(payload) {
      if (!matches(payload) || !Number.isSafeInteger(payload.request_id)) return false;
      const request = pending.get(payload.request_id);
      if (!request) return false;
      pending.delete(payload.request_id);
      if (payload.retry_ai_save && bounded(payload.content, MAX_AI_BYTES)) {
        failedAiSave = {
          anchor: request.anchor || failedAiSave?.anchor,
          content: payload.content,
          content_html: payload.content_html,
          error: payload.error || "AI 解释已生成，但保存失败。请重试保存。",
          pending: null,
        };
      } else if (["retry_ai_save", "discard_ai_save"].includes(request.action) && failedAiSave) {
        failedAiSave.pending = null;
        failedAiSave.pendingAction = null;
      }
      if (payload.notes) receiveNotes(payload.notes);
      if (editor?.pending === payload.request_id) {
        editor.pending = null;
        input.disabled = false;
        saveButton.disabled = false;
        if (payload.ok) {
          editor = null;
          reportDraft(false);
          input.value = "";
          editorPanel.hidden = true;
        } else {
          editorStatus.textContent = payload.error || "保存失败，已保留草稿，请重试。";
          input.focus({ preventScroll: true });
        }
      }
      if (payload.ok) {
        if (["ai_explain", "retry_ai_save", "discard_ai_save"].includes(request.action)) failedAiSave = null;
        if (request.action === "delete" && editor?.id === request.id) {
          editor = null;
          reportDraft(false);
          input.value = "";
          editorPanel.hidden = true;
        }
        if (request.action !== "list") {
          tell(request.action === "delete" ? "笔记已删除。" :
            request.action === "remove_mark" ? "划线已删除。" :
            request.action === "discard_ai_save" ? "已放弃未保存的 AI 想法。" :
            ["ai_explain", "retry_ai_save"].includes(request.action) ? "AI 解释已保存为 AI 想法。" : "笔记已保存。");
        }
        if (!payload.notes && request.action !== "list") post("list");
      } else {
        tell(payload.error || "笔记操作失败，请重试。", true);
      }
      refreshList();
      schedulePaint();
      return true;
    },
    state(payload) {
      if (!matches(payload)) return false;
      const request = pending.get(payload.request_id);
      if (!request || request.action !== "ai_explain") return false;
      if (bounded(payload.content, MAX_AI_BYTES)) {
        request.content = payload.content;
        request.content_html = bounded(payload.content_html, MAX_CONTENT_HTML_BYTES) ? payload.content_html : undefined;
      }
      refreshList();
      return true;
    },
    explainSelection(selectedText) {
      const selected = ready && context ?
        (typeof selectedText === "string" ? contextMenuSnapshot : currentSelection()) : null;
      if (!selected || (typeof selectedText === "string" &&
          normalize(selectedText) !== selected.anchor.quote) || !anchorRange(selected.anchor)) {
        tell("选中文字已变化，请重新选择后再使用 AI 解释。", true);
        return false;
      }
      selectionSnapshot = selected;
      contextMenuSnapshot = null;
      choose("ai_explain");
      return true;
    },
  });
  Object.defineProperty(window, "moyeAnnotations", { value: api });

  function install() {
    if (!document.body || ready) return;
    host = element("moye-reader-notes");
    // Outside body: host chrome is never part of a book-text anchor.
    host.style.cssText = "all:initial!important;position:fixed!important;inset:0!important;" +
      "z-index:2147483646!important;pointer-events:none!important;display:block!important;" +
      "direction:ltr!important;visibility:visible!important;opacity:1!important;";
    root = host.attachShadow({ mode: "closed" });
    const style = element("style");
    style.textContent = `
      :host{color-scheme:light}*,*::before,*::after{box-sizing:border-box}
      [hidden]{display:none!important}button,textarea{font:inherit}button{cursor:pointer}
      button:disabled{cursor:wait;opacity:.55}button:focus-visible,textarea:focus-visible{
        outline:3px solid #7baeff;outline-offset:2px}
      .surface{font:14px/1.5 "Microsoft YaHei",system-ui,sans-serif;color:#263345;
        letter-spacing:normal;word-spacing:normal;text-align:left;writing-mode:horizontal-tb}
      .toolbar{position:fixed;display:flex;flex-wrap:nowrap;justify-content:flex-start;gap:2px;
        width:max-content;max-width:calc(100vw - 16px);overflow-x:auto;overscroll-behavior:contain;
        scrollbar-width:thin;padding:7px;background:#303334;color:white;
        border-radius:15px;box-shadow:0 5px 18px #0004;pointer-events:auto}
      .tool{border:0;color:#f9fafb;background:transparent;border-radius:9px;
        padding:7px 8px;min-width:54px;flex:0 0 auto;display:flex;align-items:center;flex-direction:column;
        gap:3px;font-size:12px;white-space:nowrap}.tool:hover{background:#ffffff20}
      .tool-icon{height:23px;line-height:23px;font-size:21px;font-family:Arial,sans-serif}
      .tool-icon.highlight{background:#dfbd58;color:#303334;width:21px}
      .tool-icon.wavy{text-decoration:underline wavy}.tool-icon.underline{text-decoration:underline}
      .layer{position:fixed;inset:0;
        overflow:hidden;pointer-events:none}.mark{position:absolute;pointer-events:none}
      .mark.highlight{background:#f6d75855}.mark.wavy{background:radial-gradient(ellipse at
        50% 100%,transparent 43%,#cf674b 47%,#cf674b 60%,transparent 64%) 0 100%/8px 5px repeat-x}
      .mark.underline{border-bottom:2px solid #4e82b7}
      .mark.human_comment{border-bottom:2px dotted #9b79c5}
      .mark.ai_comment{border-bottom:2px dotted #359a8e}
      .toggle{position:fixed;right:18px;bottom:18px;border:1px solid #d9dfe8;
        border-radius:22px;background:#fff;color:#344052;padding:9px 17px;
        pointer-events:auto;box-shadow:0 3px 15px #0002}
      .drawer{position:fixed;right:12px;top:12px;bottom:70px;width:352px;
        max-width:calc(100vw - 24px);display:flex;flex-direction:column;overflow:hidden;
        border:1px solid #dbe1e8;border-radius:14px;background:#f8fafc;
        box-shadow:0 8px 34px #18263b33;pointer-events:auto}
      .drawer-header{display:flex;align-items:center;justify-content:space-between;
        padding:13px 16px;background:white;border-bottom:1px solid #e6e9ef}
      .drawer-title{font-size:16px;font-weight:600}.close{font-size:21px;border:0;
        background:transparent;color:#697687;padding:0 6px;line-height:27px}
      .note-scroll{overflow:auto;overscroll-behavior:contain;flex:1;min-height:60px;padding:12px}
      .list-status{color:#758194;margin:8px 3px}.note{background:white;border:1px solid #e4e8ee;
        border-radius:10px;padding:12px;margin-bottom:10px;overflow-wrap:anywhere}
      .note-header{display:flex;gap:8px;align-items:center;flex-wrap:wrap}
      .note-type{font-size:12px;font-weight:600;color:#627083}.stale{font-size:11px;color:#a16942}
      .note.human_comment .note-type{color:#8964b5}.note.ai_comment .note-type{color:#237f77}
      .quote{margin:9px 0;padding:3px 0 3px 9px;border-left:3px solid #d9dfe7;
        color:#697587;font-size:13px;white-space:pre-wrap;max-height:112px;overflow:auto}
      .content{margin:9px 0 0;white-space:pre-wrap;user-select:text}
      .markdown{white-space:normal;min-width:0;max-width:100%;line-height:1.65}
      .markdown>:first-child{margin-top:0}.markdown>:last-child{margin-bottom:0}
      .markdown p{margin:.65em 0}.markdown h1,.markdown h2,.markdown h3,.markdown h4,
      .markdown h5,.markdown h6{font-weight:650;line-height:1.4;margin:1em 0 .45em}
      .markdown h1{font-size:1.5em}.markdown h2{font-size:1.3em}.markdown h3{font-size:1.15em}
      .markdown h4,.markdown h5,.markdown h6{font-size:1em}
      .markdown ul,.markdown ol{padding-left:1.6em;margin:.6em 0}.markdown li{margin:.25em 0}
      .markdown blockquote{margin:.7em 0;padding:.2em .7em;border-left:3px solid #a8bcd0;
        color:#627083;background:#f6f8fa}.markdown strong,.markdown b{font-weight:650}
      .markdown em,.markdown i{font-style:italic}.markdown del,.markdown s{text-decoration:line-through}
      .markdown code{font-family:Consolas,"Courier New",monospace;font-size:.92em;
        background:#eef2f6;border-radius:3px;padding:.1em .25em}
      .markdown pre{margin:.7em 0;padding:10px;max-width:100%;overflow-x:auto;
        white-space:pre;overflow-wrap:normal;tab-size:4;background:#eef2f6;border-radius:6px}
      .markdown pre code{font-size:.9em;padding:0;border:0;background:none;white-space:inherit}
      .markdown table{display:block;width:max-content;max-width:100%;overflow-x:auto;
        border-collapse:collapse;margin:.8em 0;font-size:.94em;white-space:nowrap;overflow-wrap:normal}
      .markdown th,.markdown td{border:1px solid #d9e1ea;padding:6px 9px;text-align:left}
      .markdown th{background:#f1f5f9;font-weight:600}.markdown hr{margin:1em 0;border:0;
        border-top:1px solid #d9e1ea}.task-checkbox{font-size:1.1em;margin-right:.3em}
      .copy-source{position:fixed;left:-10000px;top:0;opacity:0}
      .note-actions{display:flex;flex-wrap:wrap;justify-content:flex-end;gap:12px;margin-top:9px}
      .text-button{border:0;background:transparent;color:#51739b;font-size:12px;padding:2px}
      .danger{color:#a46666}.editor{flex:none;max-height:65%;overflow:auto;padding:13px 15px;
        background:white;border-top:1px solid #dfe5ed}.editor-title{font-weight:600}
      textarea{display:block;width:100%;resize:vertical;min-height:105px;max-height:230px;
        padding:9px;border:1px solid #d3dae4;border-radius:7px;color:#263345;background:white}
      .editor-actions{display:flex;justify-content:flex-end;gap:10px;margin-top:9px}
      .save{border:0;border-radius:6px;background:#3f709a;color:white;padding:6px 14px}
      .cancel{border:1px solid #d6dee7;border-radius:6px;background:white;color:#627083;padding:6px 12px}
      .editor-status{font-size:12px;color:#758194;margin-top:7px;overflow-wrap:anywhere}
      .toast{position:fixed;left:50%;bottom:24px;transform:translateX(-50%);max-width:calc(100vw - 40px);
        padding:9px 15px;border-radius:9px;background:#263345;color:white;box-shadow:0 3px 15px #0002;
        pointer-events:none;white-space:pre-wrap;overflow-wrap:anywhere}.toast.error{background:#8c4540}
      @media(max-width:430px){.tool{min-width:41px;padding:5px}.toolbar{gap:0}.drawer{right:8px}}
    `;
    root.append(style);
    layer = element("div", "layer");
    layer.setAttribute("aria-hidden", "true");
    toolbar = element("div", "surface toolbar");
    toolbar.setAttribute("role", "toolbar");
    toolbar.setAttribute("aria-label", "选中文字操作");
    toolbar.hidden = true;
    toolbar.addEventListener("pointerdown", (event) => event.preventDefault());
    const options = [
      ["copy", "复制", "▣"], ["highlight", "马克笔", "A"],
      ["wavy", "波浪线", "A"], ["underline", "直线", "A"],
      ["remove_mark", "删除划线", ""], ["human_comment", "写想法", ""],
      ["ai_explain", "AI 解释", "✧"],
    ];
    for (const [action, label, icon] of options) {
      const tool = button("", "tool", () => {
        if (action !== "copy") { choose(action); return; }
        if (!selectionSnapshot) return;
        try {
          if (document.execCommand("copy")) tell("已复制选中文字。");
          else tell("复制失败，请使用 Ctrl + C。", true);
        } catch { tell("复制失败，请使用 Ctrl + C。", true); }
        toolbar.hidden = true;
      });
      tool.dataset.action = action;
      tool.setAttribute("aria-label", label);
      const iconNode = element("span", `tool-icon ${action}`, icon);
      iconNode.setAttribute("aria-hidden", "true");
      if (action === "human_comment" || action === "remove_mark") {
        const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
        svg.setAttribute("viewBox", "0 0 24 24");
        svg.setAttribute("width", "23");
        svg.setAttribute("height", "23");
        const outline = document.createElementNS("http://www.w3.org/2000/svg", "path");
        outline.setAttribute("d", action === "remove_mark" ?
          "M4 14l9-10 7 6-8 10H8z M9 9l7 7 M12 20h9" : "M5 4h14v12H11l-5 4v-4H5z");
        outline.setAttribute("fill", "none");
        outline.setAttribute("stroke", "currentColor");
        outline.setAttribute("stroke-width", "1.6");
        outline.setAttribute("stroke-linejoin", "round");
        svg.append(outline);
        iconNode.append(svg);
      }
      tool.append(iconNode, element("span", "", label));
      toolbar.append(tool);
    }
    toolbar.addEventListener("keydown", (event) => {
      if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
      event.preventDefault();
      const buttons = Array.from(toolbar.querySelectorAll("button"));
      let position = buttons.indexOf(root.activeElement);
      if (event.key === "Home") position = 0;
      else if (event.key === "End") position = buttons.length - 1;
      else position = (position + (event.key === "ArrowRight" ? 1 : -1) + buttons.length) % buttons.length;
      buttons[position].focus({ preventScroll: true });
    });
    drawer = element("aside", "surface drawer");
    drawer.setAttribute("aria-label", "本章笔记");
    drawer.hidden = true;
    const header = element("div", "drawer-header");
    const close = button("×", "close", closeDrawer);
    close.setAttribute("aria-label", "收起笔记，保留未保存的想法");
    drawerTitle = element("span", "drawer-title", "本章笔记");
    header.append(drawerTitle, close);
    const scroll = element("div", "note-scroll");
    listStatus = element("p", "list-status");
    list = element("div", "note-list");
    scroll.append(listStatus, list);
    editorPanel = element("section", "editor");
    editorPanel.hidden = true;
    editorTitle = element("div", "editor-title");
    quoteView = element("blockquote", "quote");
    input = element("textarea");
    input.setAttribute("aria-label", "人工想法内容");
    input.placeholder = "记录你对这段文字的想法…";
    input.addEventListener("input", () => {
      if (editor) { editor.value = input.value; reportDraft(true); }
    });
    input.addEventListener("keydown", (event) => {
      if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
        event.preventDefault(); saveEditor();
      }
    });
    const editorActions = element("div", "editor-actions");
    saveButton = button("保存", "save", saveEditor);
    editorActions.append(button("取消", "cancel", () => {
      if (editor?.pending) return;
      editor = null;
      reportDraft(false);
      input.value = "";
      editorPanel.hidden = true;
    }), saveButton);
    editorStatus = element("div", "editor-status");
    editorStatus.setAttribute("role", "status");
    editorPanel.append(editorTitle, quoteView, input, editorActions, editorStatus);
    drawer.append(header, scroll, editorPanel);
    toggle = button("笔记 0", "surface toggle", () => {
      if (drawer.hidden || noteScope !== null) openDrawer(null);
      else closeDrawer();
    });
    toggle.setAttribute("aria-expanded", "false");
    toggle.title = "本章笔记（Ctrl + Alt + N）";
    toast = element("div", "surface toast");
    toast.setAttribute("role", "status");
    toast.hidden = true;
    root.append(layer, toolbar, drawer, toggle, toast);
    document.documentElement.append(host);
    ready = true;
    refreshList();
    schedulePaint();
    if (context) post("list");

    document.addEventListener("selectionchange", () => {
      if (root.activeElement === input || root.activeElement?.closest(".toolbar")) return;
      clearTimeout(selectionTimer);
      selectionTimer = setTimeout(showSelection, 80);
    });
    document.addEventListener("contextmenu", (event) => {
      // Native context menus may collapse Selection before their command arrives.
      // Freeze the actual range now; the host passes back the same selected text.
      // A notes menu must also invalidate a previous book-text snapshot.
      contextMenuSnapshot = event.target === host ? null : currentSelection();
    }, true);
    document.addEventListener("click", (event) => {
      const link = event.target.closest?.("a[href]");
      if (!link || !document.body.contains(link)) return;
      const busy = Array.from(pending.values()).some((request) => request.action !== "list");
      if (!editor && !busy && !failedAiSave) return;
      event.preventDefault();
      tell(editor ? "请先保存或取消当前人工想法，再打开链接。" :
        failedAiSave ? "AI 想法尚未保存，请先重试保存或放弃。" :
        "笔记正在处理，请完成或取消 AI 生成后再打开链接。", true);
    }, true);
    document.addEventListener("pointerup", (event) => {
      if (event.target === host || event.button !== 0) return;
      clearTimeout(selectionTimer);
      // The click also emits selectionchange. Its toolbar debounce must not
      // cancel the independent annotation hit test.
      clearTimeout(hitTimer);
      hitTimer = setTimeout(() => {
        showSelection();
        if (window.getSelection()?.isCollapsed && !event.target.closest?.("a,button,input,textarea")) {
          const hits = hitAreas.filter(({ rect }) => event.clientX >= rect.left &&
            event.clientX <= rect.right && event.clientY >= rect.top && event.clientY <= rect.bottom);
          if (hits.length) {
            const ids = new Set(hits.map((hit) => hit.id));
            const anchors = notes.filter((note) => ids.has(note.id)).map((note) => note.anchor);
            if (hasRelatedThought(anchors)) {
              openDrawer(anchors);
              const card = Array.from(list.children).find((node) => node.dataset.noteId === hits[0].id);
              card?.scrollIntoView({ block: "nearest" });
            } else {
              selectMarkedRange(hits[0].id);
            }
          }
        }
      }, 40);
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        toolbar.hidden = true;
        if (!drawer.hidden) { event.preventDefault(); closeDrawer(); }
      }
      if (event.ctrlKey && event.altKey && event.key.toLowerCase() === "n") {
        event.preventDefault(); openDrawer(null);
        drawer.querySelector("button")?.focus({ preventScroll: true });
      }
      if (event.altKey && event.key === "Enter" && currentSelection()) {
        event.preventDefault(); showSelection(); toolbar.querySelector("button")?.focus();
      }
    });
    document.addEventListener("scroll", schedulePaint, { capture: true, passive: true });
    window.addEventListener("resize", schedulePaint, { passive: true });
    document.addEventListener("load", schedulePaint, true);
    if (document.fonts) document.fonts.ready.then(schedulePaint);
    new ResizeObserver(schedulePaint).observe(document.body);
    new MutationObserver(() => { index = null; schedulePaint(); }).observe(document.body, {
      childList: true, characterData: true, subtree: true,
    });
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", install, { once: true });
  else install();
  window.addEventListener("pagehide", () => {
    clearTimeout(selectionTimer);
    clearTimeout(hitTimer);
    clearTimeout(toastTimer);
    cancelAnimationFrame(paintFrame);
    pending.clear();
    context = null;
  }, { once: true });
})();
