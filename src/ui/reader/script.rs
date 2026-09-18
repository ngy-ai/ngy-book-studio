//! The webview CSP and the initialization script injected into every reader
//! page.

pub(crate) const READER_CSP: &str = "default-src 'self' data:; script-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; media-src 'self' data:";
pub(crate) const READER_INITIALIZATION_SCRIPT: &str = r#"
(() => {
  "use strict";
  const MAX_SELECTION_BYTES = 32768;
  const encoder = new TextEncoder();
  let timer = 0;

  const ownDocument = () => {
    const url = new URL(window.location.href);
    return window.top === window && (
      (url.protocol === "epubreader:" && url.hostname === "book") ||
      ((url.protocol === "http:" || url.protocol === "https:") &&
        url.hostname === "epubreader.book")
    );
  };

  const isChapterNode = (node) => node && node.getRootNode() === document &&
    document.body?.contains(node);

  const insideTranslation = (node) => {
    const element = node && (node.nodeType === 1 ? node : node.parentElement);
    return !!(element && element.closest?.("[data-ngy-translation]"));
  };

  // Reading-time translations are a display layer. Selections and AI references
  // must only ever contain the immutable original book text.
  const bookText = (range) => {
    const fragment = range.cloneContents();
    if (fragment.querySelectorAll) {
      for (const node of Array.from(fragment.querySelectorAll("[data-ngy-translation]"))) {
        node.remove();
      }
    }
    return fragment.textContent || "";
  };

  // A selection made on the translation layer stands for the original passage it
  // was translated from; the translation runtime owns that mapping and returns
  // null when the selection touches no applied translation.
  const translationOriginalRange = (range) => {
    const resolve = window.ngyTranslations?.originalRange;
    return typeof resolve === "function" ? resolve(range) : null;
  };

  const boundedSelection = () => {
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || selection.rangeCount !== 1) return "";
    // Notes live in a closed shadow root outside body. Their text (including
    // an identical quote) is never a chapter selection or an AI reference.
    let range = selection.getRangeAt(0);
    if (range.collapsed || ![selection.anchorNode, selection.focusNode,
        range.startContainer, range.endContainer].every(isChapterNode)) return "";
    if ([range.startContainer, range.endContainer].some(insideTranslation)) {
      const original = translationOriginalRange(range);
      if (!original) return "";
      range = original;
    }
    // Focusing a notes input can leave the previous body Range in Selection.
    const active = document.activeElement;
    if (active && (!isChapterNode(active) ||
        active.matches("input,textarea,select,[contenteditable='true']"))) return "";
    const value = bookText(range)
      .replace(/\s+/gu, " ")
      .trim();
    if (!value) return "";
    if (encoder.encode(value).byteLength <= MAX_SELECTION_BYTES) return value;
    let low = 0;
    let high = value.length;
    while (low < high) {
      const middle = Math.ceil((low + high) / 2);
      if (encoder.encode(value.slice(0, middle)).byteLength <= MAX_SELECTION_BYTES) {
        low = middle;
      } else {
        high = middle - 1;
      }
    }
    return value.slice(0, low).trim();
  };

  const send = () => {
    window.clearTimeout(timer);
    if (!ownDocument()) return;
    try {
      window.ipc?.postMessage(JSON.stringify({
        type: "selection_changed",
        selected_text: boundedSelection(),
      }));
    } catch {
      // A closed host simply drops the transient selection.
    }
  };

  document.addEventListener("selectionchange", () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(send, 80);
  });
  // focusin is composed, unlike a textarea's own selectionchange in a closed
  // shadow root. Clear the old transient highlight before a queued send fires.
  document.addEventListener("focusin", () => {
    const active = document.activeElement;
    if (active && (!isChapterNode(active) ||
        active.matches("input,textarea,select,[contenteditable='true']"))) send();
  }, true);
  window.addEventListener("pagehide", () => window.clearTimeout(timer), { once: true });

  // Ctrl + wheel resizes the reading text. The size is one custom property on
  // the root element, so the chapter reflows instead of the WebView zooming;
  // the host remembers it per book and the range matches
  // `ngy_book_studio::reader`, which clamps the same value when it is applied.
  const MIN_TEXT_SIZE = 14;
  const MAX_TEXT_SIZE = 30;
  const TEXT_SIZE_STEP = 2;
  // Wheel notches arrive in bursts; report the size once the reader stops.
  const TEXT_SIZE_REPORT_MS = 250;
  let sizeTimer = 0;

  // The reader stylesheet always defines the size; a document without it is not
  // a chapter and must not report a size of its own.
  const currentTextSize = () => {
    const applied = parseFloat(getComputedStyle(document.documentElement)
      .getPropertyValue("--ngy-font-size"));
    return Number.isFinite(applied) ? applied : 0;
  };

  const applyTextSize = (size) => {
    document.documentElement.style.setProperty("--ngy-font-size", `${size}px`);
  };

  const reportTextSize = () => {
    window.clearTimeout(sizeTimer);
    const size = currentTextSize();
    if (!ownDocument() || size === 0) return;
    try {
      window.ipc?.postMessage(JSON.stringify({
        type: "reader_text_size",
        font_size: Math.round(size),
      }));
    } catch {
      // A closed host simply drops the size; the next chapter re-applies it.
    }
  };

  window.addEventListener("wheel", (event) => {
    if (!event.ctrlKey || event.altKey || event.metaKey || event.shiftKey) return;
    if (event.deltaY === 0) return;
    const size = currentTextSize();
    if (size === 0) return;
    // Held Ctrl means "resize", so the chapter must not also scroll under the
    // pointer by the same notch.
    event.preventDefault();
    if (!ownDocument()) return;
    const stepped = size + (event.deltaY < 0 ? TEXT_SIZE_STEP : -TEXT_SIZE_STEP);
    applyTextSize(Math.min(MAX_TEXT_SIZE, Math.max(MIN_TEXT_SIZE, stepped)));
    window.clearTimeout(sizeTimer);
    sizeTimer = window.setTimeout(reportTextSize, TEXT_SIZE_REPORT_MS);
  }, { passive: false });

  window.addEventListener("pagehide", () => window.clearTimeout(sizeTimer), { once: true });
})();
"#;
