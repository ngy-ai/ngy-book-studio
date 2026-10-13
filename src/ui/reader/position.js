// Chapter reading position: remembers where the reader stopped inside a
// chapter and scrolls back there when the host reopens the book.
//
// Offsets use the note coordinate system: UTF-16 code units of the chapter's
// DOM text after ECMAScript whitespace is removed, plus a short quote that
// both sides use to confirm the anchor still names the same passage.
(() => {
  "use strict";

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
  // A position marker, not a selection: short enough for the host to bound and
  // store with every progress write.
  const QUOTE_CODE_UNITS = 24;
  const MAX_QUOTE_BYTES = 512;
  // Scrolling settles before the position is reported.
  const REPORT_DELAY_MS = 400;
  // Sample rows below the viewport top when looking for the visible character;
  // skipped sampling columns and rows fall through to the next candidate.
  const SCAN_ROW_STEP = 16;
  const MAX_SCAN_ROWS = 16;
  const SAMPLE_X = [0.5, 0.3, 0.7];
  const EXCLUDED = "script,style,noscript,template";

  const compact = (value) => value.replace(/\s/gu, "");

  const viewport = () => ({
    width: document.documentElement.clientWidth || window.innerWidth || 0,
    height: document.documentElement.clientHeight || window.innerHeight || 0,
  });

  const insideTranslation = (node) => {
    const parent = node.parentElement;
    return !!(parent && parent.closest("[data-ngy-translation]"));
  };

  const blocked = (node) => {
    const parent = node.parentElement;
    return !parent || !!parent.closest(EXCLUDED);
  };

  // The chapter text in the note coordinate system: every original text node in
  // document order minus ECMAScript whitespace. Translation layers are a
  // display artifact and never part of the text a position is anchored to.
  const buildIndex = () => {
    if (!document.body) return { entries: [], text: "" };
    const entries = [];
    let text = "";
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        if (!node.data || blocked(node) || insideTranslation(node)) {
          return NodeFilter.FILTER_REJECT;
        }
        return NodeFilter.FILTER_ACCEPT;
      },
    });
    let node;
    while ((node = walker.nextNode())) {
      const compressed = compact(node.data);
      if (!compressed) continue;
      entries.push({
        node,
        data: node.data,
        start: text.length,
        end: text.length + compressed.length,
      });
      text += compressed;
    }
    return { entries, text };
  };

  // Whitespace-free UTF-16 offset of a DOM point, or null when the point is not
  // part of the indexed chapter text.
  const globalOffsetOf = (index, node, nodeOffset) => {
    let prefix = 0;
    for (const entry of index.entries) {
      if (entry.node === node) {
        return prefix + compact(entry.data.slice(0, nodeOffset)).length;
      }
      prefix = entry.end;
    }
    return null;
  };

  // The DOM point of a whitespace-free UTF-16 offset, or null when out of range.
  const pointForOffset = (index, offset) => {
    for (const entry of index.entries) {
      if (offset < entry.start || offset > entry.end) continue;
      let remaining = offset - entry.start;
      const data = entry.data;
      for (let position = 0; position < data.length; position += 1) {
        if (/\s/u.test(data[position])) continue;
        if (remaining === 0) return { node: entry.node, offset: position };
        remaining -= 1;
      }
      return remaining === 0 ? { node: entry.node, offset: data.length } : null;
    }
    return null;
  };

  // The line box a DOM point sits on. A collapsed range reports the caret
  // rectangle; an engine that reports none falls back to the block rectangle.
  const caretRect = (node, offset) => {
    const range = document.createRange();
    range.setStart(node, offset);
    range.collapse(true);
    const rect = range.getBoundingClientRect();
    if (rect && rect.height > 0) return rect;
    const parent = node.parentElement;
    const block = parent ? parent.getBoundingClientRect() : null;
    return block && block.height > 0 ? block : null;
  };

  const caretAt = (x, y) => {
    if (typeof document.caretRangeFromPoint === "function") {
      return document.caretRangeFromPoint(x, y);
    }
    if (typeof document.caretPositionFromPoint === "function") {
      const position = document.caretPositionFromPoint(x, y);
      if (!position) return null;
      const range = document.createRange();
      range.setStart(position.offsetNode, position.offset);
      range.collapse(true);
      return range;
    }
    return null;
  };

  const firstTextNode = (node) => {
    if (node.nodeType === Node.TEXT_NODE) return node;
    const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT, null);
    return walker.nextNode();
  };

  // A point on the translation layer stands for the original passage it was
  // built from; positions must stay anchored to the original text.
  const mappedOriginalPoint = (node, offset) => {
    const resolve = window.ngyTranslations?.originalRange;
    if (typeof resolve !== "function") return null;
    const range = document.createRange();
    range.setStart(node, offset);
    range.collapse(true);
    const original = resolve(range);
    if (!original) return null;
    const mapped = firstTextNode(original.startContainer);
    if (!mapped) return null;
    return { node: mapped, offset: mapped === original.startContainer ? original.startOffset : 0 };
  };

  const sampledPoint = (x, y, index, height) => {
    const range = caretAt(x, y);
    if (!range) return null;
    const node = range.startContainer;
    const offset = range.startOffset;
    if (node.nodeType !== Node.TEXT_NODE || !node.data) return null;
    if (insideTranslation(node)) {
      const mapped = mappedOriginalPoint(node, offset);
      return mapped && index.entries.some((entry) => entry.node === mapped.node) ? mapped : null;
    }
    if (blocked(node) || !index.entries.some((entry) => entry.node === node)) return null;
    // The sampled caret must really be on screen: a point far from any text is
    // clamped to the nearest line by the engine, which is not the position the
    // reader is looking at.
    const rect = caretRect(node, offset);
    if (!rect || rect.bottom <= 0 || rect.top >= height) return null;
    return { node, offset };
  };

  const boundedQuote = (text, start) => {
    if (start >= text.length) return "";
    let end = Math.min(text.length, start + QUOTE_CODE_UNITS);
    // Never cut between the halves of a surrogate pair.
    const last = text.charCodeAt(end - 1);
    if (last >= 0xd800 && last <= 0xdbff) end -= 1;
    if (end <= start) return "";
    const quote = text.slice(start, end);
    return encoder.encode(quote).byteLength <= MAX_QUOTE_BYTES ? quote : "";
  };

  const anchorAt = (index, node, offset) => {
    const start = globalOffsetOf(index, node, offset);
    if (start === null) return null;
    const quote = boundedQuote(index.text, start);
    return quote ? { quote, start, end: start + quote.length } : null;
  };

  // The position of the first visible character below the viewport top, as the
  // reader sees it.
  const currentAnchor = () => {
    const index = buildIndex();
    if (!index.text) return null;
    const { width, height } = viewport();
    if (width <= 0 || height <= 0) return null;
    const bottom = Math.min(height, MAX_SCAN_ROWS * SCAN_ROW_STEP);
    for (let y = 0; y <= bottom; y += SCAN_ROW_STEP) {
      for (const ratio of SAMPLE_X) {
        const x = Math.max(1, Math.min(width - 1, Math.round(width * ratio)));
        const point = sampledPoint(x, y, index, height);
        if (!point) continue;
        const anchor = anchorAt(index, point.node, point.offset);
        if (anchor) return anchor;
      }
    }
    // Nothing sampled (images or blank space fill the top of the viewport):
    // fall back to the first text block that is on screen.
    for (const entry of index.entries) {
      const parent = entry.node.parentElement;
      const rect = parent ? parent.getBoundingClientRect() : null;
      if (!rect || rect.bottom <= 0 || rect.top >= height) continue;
      const anchor = anchorAt(index, entry.node, 0);
      if (anchor) return anchor;
    }
    return null;
  };

  let reportTimer = 0;
  let lastReported = null;

  const send = () => {
    window.clearTimeout(reportTimer);
    reportTimer = 0;
    if (!ownDocument()) return;
    const anchor = currentAnchor();
    if (!anchor) return;
    if (lastReported && lastReported.start === anchor.start &&
        lastReported.quote === anchor.quote) {
      return;
    }
    lastReported = anchor;
    try {
      window.ipc?.postMessage(JSON.stringify({ type: "reading_position", anchor }));
    } catch {
      // A closed host simply drops the position.
    }
  };

  const schedule = () => {
    window.clearTimeout(reportTimer);
    reportTimer = window.setTimeout(send, REPORT_DELAY_MS);
  };

  // Scrolls the saved position back to the top of the viewport. The anchor is
  // confirmed against the current chapter text first: a chapter that changed
  // since the position was saved stays where it is instead of guessing.
  const restore = (anchor) => {
    if (!anchor || typeof anchor.quote !== "string" || anchor.quote.length === 0) return false;
    if (!Number.isSafeInteger(anchor.start) || !Number.isSafeInteger(anchor.end)) return false;
    if (anchor.start < 0 || anchor.end <= anchor.start) return false;
    const index = buildIndex();
    if (anchor.end > index.text.length) return false;
    if (index.text.slice(anchor.start, anchor.end) !== anchor.quote) return false;
    const point = pointForOffset(index, anchor.start);
    if (!point) return false;
    const rect = caretRect(point.node, point.offset);
    if (!rect) return false;
    lastReported = { quote: anchor.quote, start: anchor.start, end: anchor.end };
    window.scrollTo({ top: Math.max(0, window.scrollY + rect.top), behavior: "auto" });
    return true;
  };

  window.addEventListener("scroll", schedule, { passive: true });
  window.addEventListener("pagehide", () => {
    window.clearTimeout(reportTimer);
    send();
  }, { once: true });

  window.ngyPosition = Object.freeze({ restore, current: currentAnchor, send });
})();
