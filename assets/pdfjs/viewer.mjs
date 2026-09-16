import {
  AnnotationMode,
  GlobalWorkerOptions,
  TextLayer,
  getDocument,
} from "./pdf.mjs";

const PDFJS_VERSION = "5.7.284";
const MAX_PDF_BYTES = 512 * 1024 * 1024;
const MAX_RENDER_PAGES = 20_000;
const MAX_TEXT_LAYER_ITEMS = 100_000;
const MAX_SELECTION_BYTES = 32 * 1024;
const MAX_SELECTION_SCAN_CODE_UNITS = 128 * 1024;
const SELECTION_SEPARATOR = /[\u0000-\u001f\u007f-\u009f\s]/u;
// Ctrl + wheel changes the page scale. The host owns the scale of a book, so it
// pushes one here; this shell only steps it, keeps the page being read in place
// while the column re-lays out, and reports back the scale it stopped at. The
// bounds are the host's (`src/ui/pdf_reader.rs`), which rejects anything else.
const PAGE_ZOOM_DEFAULT_MILLI = 1500;
const PAGE_ZOOM_MIN_MILLI = 500;
const PAGE_ZOOM_MAX_MILLI = 4000;
const PAGE_ZOOM_STEP_MILLI = 250;
// Wheel notches arrive in bursts; report the scale once the reader stops.
const PAGE_ZOOM_REPORT_MS = 250;
// Pages are laid out immediately but only drawn close to the viewport, then
// released again so a very long document never keeps the whole book in memory.
const RENDER_ROOT_MARGIN = "200% 0px";
// Pages further than this from the page being read go back to placeholders.
const KEEP_RENDERED_RADIUS = 8;
// The host owns page state, so a scroll only reports the page it settled on.
const PAGE_CHANGE_SETTLE_MS = 150;
const PAGE_TOP_PADDING = 12;
const status = document.querySelector("#status");
const pages = document.querySelector("#pages");
const originalFetch = globalThis.fetch.bind(globalThis);
const utf8Encoder = new TextEncoder();
let activeLoadingTask = null;
let openedPdf = null;
let documentGeneration = 0;
// Last `ngy-pdf-go-to` request id. Every page change and selection reuses it
// so the host can still reject messages from a superseded navigation.
let requestId = 0;
let currentPage = 0;
let reportedPage = 0;
let slots = [];
let slotsByPage = new Map();
const renderedPages = new Set();
const renderQueue = [];
let renderQueueRunning = false;
let pageObserver = null;
let viewportEstimate = null;
let frameHandle = 0;
let settleTimer = 0;
let zoomReportTimer = 0;
let pageZoomMilli = PAGE_ZOOM_DEFAULT_MILLI;
let lastSelectionPayload = null;
let lastStatusPage = 0;

GlobalWorkerOptions.workerSrc = "./pdf.worker.mjs";

// Page spacing is a host preference. `compact=1` removes the gap between pages
// before the first page slot is built, so the reading column never jumps; the
// host later toggles the very same attribute without reloading the document.
function applyCompactReading() {
  if (new URLSearchParams(location.search).get("compact") === "1") {
    document.documentElement.setAttribute("data-pdf-compact", "1");
  } else {
    document.documentElement.removeAttribute("data-pdf-compact");
  }
}

applyCompactReading();

// The host toggles the same attribute on a live document, so removing or
// restoring the page gap deletes or adds height above the page being read, which
// would scroll the document under the reader. Measure the exact drift and
// correct what the engine did not already preserve: the previous layout is
// measured by briefly restoring the attribute, which happens inside one
// synchronous task and therefore never paints or flickers. Correcting only the
// remainder is what keeps this from doubling up with browser scroll anchoring.
const compactObserver = new MutationObserver(keepAnchorAfterLayoutChange);
let keepingAnchor = false;

/// The page the reader is looking at, drawn or not: the pinned draft page wins,
/// then the page covering the viewport centre. `currentPage` only tracks pages
/// that are already drawn, so while a newly reached page is still a placeholder
/// it lags behind the page the reader actually sees and must not decide where
/// the reading position is kept.
function anchorSlot() {
  const locked = bridgeLockedPage();
  if (locked) {
    const pinned = slotsByPage.get(locked);
    if (pinned) return pinned;
  }
  const center = window.innerHeight / 2;
  for (const slot of slotsByPage.values()) {
    const rect = slot.container.getBoundingClientRect();
    if (rect.top <= center && rect.bottom >= center) return slot;
  }
  return slotsByPage.get(currentPage) || null;
}

function keepAnchorAfterLayoutChange() {
  if (keepingAnchor) return;
  const slot = anchorSlot();
  if (!slot) return;
  const root = document.documentElement;
  const compact = root.hasAttribute("data-pdf-compact");
  keepingAnchor = true;
  let drift = null;
  try {
    root.toggleAttribute("data-pdf-compact", !compact);
    const before = slot.container.getBoundingClientRect().top;
    root.toggleAttribute("data-pdf-compact", compact);
    drift = slot.container.getBoundingClientRect().top - before;
    // Our own two toggles are not a host change and must not be re-measured.
    compactObserver.takeRecords();
  } finally {
    root.toggleAttribute("data-pdf-compact", compact);
    keepingAnchor = false;
  }
  if (drift === null || !Number.isFinite(drift) || Math.abs(drift) < 0.5) return;
  window.scrollBy(0, drift);
}

compactObserver.observe(document.documentElement, {
  attributes: true,
  attributeFilter: ["data-pdf-compact"],
});

// PDF.js only receives bytes from the trusted host. Same-origin fetch remains
// available for bundled CMaps/fonts/Wasm; CSP independently blocks every
// network origin.
globalThis.fetch = (input, init) => {
  // `URL` instances expose their absolute address as `href`, while only
  // `Request` instances use `url`. Reading `.url` from a `URL` yields
  // `undefined`, which `new URL` would resolve into a same-origin
  // "/undefined" request that fails with a confusing 404.
  const raw =
    typeof input === "string"
      ? input
      : input instanceof URL
        ? input.href
        : input.url;
  if (!raw) {
    return Promise.reject(new Error("network access is disabled in PDF preview"));
  }
  const target = new URL(raw, location.href);
  if (target.origin !== location.origin) {
    return Promise.reject(new Error("network access is disabled in PDF preview"));
  }
  return originalFetch(target, init);
};
globalThis.WebSocket = class DisabledWebSocket {
  constructor() { throw new Error("network access is disabled in PDF preview"); }
};
globalThis.EventSource = class DisabledEventSource {
  constructor() { throw new Error("network access is disabled in PDF preview"); }
};
if (navigator.sendBeacon) {
  navigator.sendBeacon = () => false;
}
window.open = () => null;
document.addEventListener("click", (event) => {
  if (event.target.closest("a")) event.preventDefault();
});
document.addEventListener("submit", (event) => event.preventDefault());

function notify(message) {
  const payload = JSON.stringify({ pdfjsVersion: PDFJS_VERSION, ...message });
  if (globalThis.ipc && typeof globalThis.ipc.postMessage === "function") {
    globalThis.ipc.postMessage(payload);
  }
}

// The notes bridge is injected before this module and installs on
// DOMContentLoaded, so every call tolerates it not being ready yet.
function bridge() {
  return globalThis.ngyAnnotations;
}

function bridgeLockedPage() {
  const value = bridge()?.lockedPage?.();
  return Number.isSafeInteger(value) && value >= 1 ? value : null;
}

function publishRenderedPages() {
  bridge()?.setPages?.([...renderedPages].sort((left, right) => left - right));
}

function boundedNormalizedSelection(value) {
  let result = "";
  let bytes = 0;
  let scannedCodeUnits = 0;
  let separated = false;
  for (const character of String(value || "")) {
    scannedCodeUnits += character.length;
    if (scannedCodeUnits > MAX_SELECTION_SCAN_CODE_UNITS) break;
    if (SELECTION_SEPARATOR.test(character)) {
      separated = result.length > 0;
      continue;
    }
    if (separated) {
      if (bytes + 1 > MAX_SELECTION_BYTES) break;
      result += " ";
      bytes += 1;
      separated = false;
    }
    const characterBytes = utf8Encoder.encode(character).byteLength;
    if (bytes + characterBytes > MAX_SELECTION_BYTES) break;
    result += character;
    bytes += characterBytes;
  }
  return result;
}

function pageContainerOf(node) {
  if (!node) return null;
  const element = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
  return element?.closest?.(".pdf-page") || null;
}

/// The single rendered page a live selection belongs to, or null.
function selectionPageRange() {
  const selection = document.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount !== 1) return null;
  const range = selection.getRangeAt(0);
  const container = pageContainerOf(range.startContainer);
  if (!container || container !== pageContainerOf(range.endContainer)) return null;
  const layer = container.querySelector(".textLayer");
  if (!layer || !layer.contains(range.startContainer) || !layer.contains(range.endContainer)) {
    return null;
  }
  return { page: Number(container.dataset.page) || 0, range };
}

function notifySelectionChanged(force = false) {
  if (!openedPdf) return;
  const found = selectionPageRange();
  const page = found ? found.page : reportedPage || currentPage;
  if (page < 1) return;
  const selectedText = found
    ? boundedNormalizedSelection(document.getSelection().toString())
    : "";
  const payload = JSON.stringify([requestId, page, selectedText]);
  if (!force && payload === lastSelectionPayload) return;
  lastSelectionPayload = payload;
  notify({
    type: "ngy-pdf-selection-changed",
    requestId,
    pageNumber: page,
    selectedText,
  });
}

document.addEventListener("selectionchange", () => notifySelectionChanged());

function bytesFromHost(value) {
  let bytes;
  if (value instanceof Uint8Array) bytes = value;
  else if (value instanceof ArrayBuffer) bytes = new Uint8Array(value);
  else if (Array.isArray(value)) bytes = Uint8Array.from(value);
  else throw new TypeError("PDF data must be bytes, never a URL");
  if (bytes.byteLength === 0 || bytes.byteLength > MAX_PDF_BYTES) {
    throw new RangeError("PDF byte length is outside the safety limit");
  }
  const header = String.fromCharCode(...bytes.subarray(0, 5));
  if (header !== "%PDF-") throw new TypeError("invalid PDF header");
  return bytes;
}

function reportError(error) {
  status.textContent = "PDF 预览失败";
  notify({
    type: "ngy-pdf-error",
    requestId,
    message: String(error?.message || error).slice(0, 1000),
  });
}

function clampPage(pageNumber) {
  const count = openedPdf?.numPages ?? 0;
  if (count < 1) return 0;
  return Math.min(count, Math.max(1, Number(pageNumber) || 1));
}

function applySize(container, viewport) {
  container.style.width = `${Math.ceil(viewport.width)}px`;
  container.style.height = `${Math.ceil(viewport.height)}px`;
  container.style.setProperty("--total-scale-factor", String(viewport.scale));
}

function clampPageZoom(zoomMilli) {
  const value = Number(zoomMilli);
  if (!Number.isFinite(value)) return PAGE_ZOOM_DEFAULT_MILLI;
  return Math.min(PAGE_ZOOM_MAX_MILLI, Math.max(PAGE_ZOOM_MIN_MILLI, Math.round(value)));
}

function pageScale() {
  return pageZoomMilli / 1_000;
}

/// One page's own box, in the units a viewport at scale 1 would measure. A
/// rotated page keeps its rotation: only the ratio to the viewport it came from
/// is kept, so the box can be re-scaled without asking PDF.js again.
function pageBoxOf(viewport) {
  const scale = viewport.scale || 1;
  return { width: viewport.width / scale, height: viewport.height / scale };
}

function sizedViewport(box) {
  return {
    width: box.width * pageScale(),
    height: box.height * pageScale(),
    scale: pageScale(),
  };
}

/// Sizes one page slot from its own box. A page whose size is not resolved yet
/// has no box and keeps whatever the estimate gave it.
function applySlotSize(slot) {
  if (slot.pageBox) applySize(slot.container, sizedViewport(slot.pageBox));
}

/// Where the reader is looking: the page at the top of the reading area and how
/// far into that page the top of the window sits. A scale change puts this point
/// back where it was instead of scrolling to a different page.
function readingAnchor() {
  const slot = anchorSlot();
  if (!slot) return null;
  const rect = slot.container.getBoundingClientRect();
  const height = rect.height || 1;
  return {
    page: slot.page,
    fraction: Math.min(1, Math.max(0, -rect.top / height)),
  };
}

function restoreReadingAnchor(anchor) {
  if (!anchor) return;
  const slot = slotsByPage.get(anchor.page);
  if (!slot) return;
  const rect = slot.container.getBoundingClientRect();
  const top = rect.top + window.scrollY + rect.height * anchor.fraction;
  window.scrollTo({ top: Math.max(0, top), behavior: "auto" });
}

/// Drops everything a slot currently holds so it can be laid out and drawn
/// again: a cancelled render, a released canvas and the placeholder it falls
/// back to. Re-observing re-reports the intersection state, so a page that is
/// still on screen is drawn again instead of staying blank.
function resetSlot(slot) {
  slot.task?.cancel();
  slot.textTask?.cancel();
  slot.task = null;
  slot.textTask = null;
  slot.container.replaceChildren();
  slot.placeholder = placeholderFor(slot.page);
  slot.container.append(slot.placeholder);
  slot.container.dataset.rendered = "false";
  slot.state = "idle";
  renderedPages.delete(slot.page);
  bridge()?.pageReleased?.(slot.page);
  if (pageObserver) {
    pageObserver.unobserve(slot.container);
    pageObserver.observe(slot.container);
  }
}

/// Re-lays out every page at the current scale.
///
/// Sizing is one style write per page, which is what keeps the whole column —
/// and therefore the scrollbar and the reading position — correct without
/// asking PDF.js for every viewport again. Only a page that actually holds
/// something has to be reset; a placeholder only needs its new size.
function applyScaleToSlots() {
  for (const slot of slots) {
    if (slot.state !== "idle") resetSlot(slot);
  }
  for (const slot of slots) applySlotSize(slot);
  publishRenderedPages();
}

function placeholderFor(pageNumber) {
  const placeholder = document.createElement("div");
  placeholder.className = "page-placeholder";
  placeholder.textContent = `第 ${pageNumber} 页`;
  return placeholder;
}

async function seedViewportEstimate() {
  const first = await openedPdf.getPage(1);
  viewportEstimate = first.getViewport({ scale: pageScale() });
  first.cleanup();
}

function createSlots(count) {
  slots = [];
  slotsByPage = new Map();
  renderedPages.clear();
  const fragment = document.createDocumentFragment();
  for (let page = 1; page <= count; page += 1) {
    const container = document.createElement("section");
    container.className = "pdf-page";
    container.dataset.page = String(page);
    container.dataset.rendered = "false";
    const placeholder = placeholderFor(page);
    container.append(placeholder);
    const pageBox = viewportEstimate ? pageBoxOf(viewportEstimate) : null;
    if (pageBox) applySize(container, sizedViewport(pageBox));
    fragment.append(container);
    const slot = {
      page,
      container,
      placeholder,
      /// This page's own box, resolved once its viewport is known.
      pageBox,
      state: "idle",
      sizeKnown: false,
      // Whether the page is inside the (expanded) render window. Only a page
      // that left it may be released, so the render margin and the release
      // radius can never fight over the same page.
      visible: false,
      task: null,
      textTask: null,
    };
    slots.push(slot);
    slotsByPage.set(page, slot);
  }
  pages.replaceChildren(fragment);
  for (const slot of slots) pageObserver.observe(slot.container);
}

async function resolveSize(slot) {
  if (slot.sizeKnown || !openedPdf) return;
  const page = await openedPdf.getPage(slot.page);
  slot.pageBox = pageBoxOf(page.getViewport({ scale: pageScale() }));
  slot.sizeKnown = true;
  applySlotSize(slot);
}

function requestRender(pageNumber, urgent = false) {
  const slot = slotsByPage.get(pageNumber);
  // A page that failed once is retried when it comes back into range.
  if (!slot || (slot.state !== "idle" && slot.state !== "failed")) return;
  slot.state = "pending";
  if (urgent) renderQueue.unshift(slot);
  else renderQueue.push(slot);
  void pumpRenderQueue();
}

async function pumpRenderQueue() {
  if (renderQueueRunning) return;
  renderQueueRunning = true;
  try {
    while (renderQueue.length) {
      const slot = renderQueue.shift();
      if (slot.state !== "pending") continue;
      const generation = documentGeneration;
      try {
        await renderSlot(slot, generation);
      } catch (error) {
        if (generation !== documentGeneration) continue;
        slot.state = "failed";
        if (slot.placeholder) {
          slot.placeholder.classList.add("error");
          slot.placeholder.textContent = `第 ${slot.page} 页渲染失败`;
        }
        reportError(error);
      }
    }
  } finally {
    renderQueueRunning = false;
    // A reopen may have queued pages while this loop was still finishing.
    if (renderQueue.length) void pumpRenderQueue();
  }
}

async function renderSlot(slot, generation) {
  if (!openedPdf) return;
  slot.state = "rendering";
  const page = await openedPdf.getPage(slot.page);
  if (generation !== documentGeneration) return;
  const viewport = page.getViewport({ scale: pageScale() });
  slot.pageBox = pageBoxOf(viewport);
  applySize(slot.container, viewport);
  slot.sizeKnown = true;
  const canvas = document.createElement("canvas");
  canvas.className = "page-canvas";
  canvas.dataset.page = String(slot.page);
  canvas.width = Math.ceil(viewport.width);
  canvas.height = Math.ceil(viewport.height);
  slot.container.append(canvas);
  const context = canvas.getContext("2d", { alpha: false });
  const renderTask = page.render({
    canvasContext: context,
    viewport,
    annotationMode: AnnotationMode.DISABLE,
    intent: "display",
  });
  slot.task = renderTask;
  try {
    await renderTask.promise;
  } catch (error) {
    if (generation !== documentGeneration || error?.name === "RenderingCancelledException") {
      return;
    }
    throw error;
  } finally {
    if (slot.task === renderTask) slot.task = null;
  }
  if (generation !== documentGeneration) return;

  const textContent = await page.getTextContent({
    includeMarkedContent: true,
    disableNormalization: false,
  });
  if (generation !== documentGeneration) return;
  if (textContent.items.length > MAX_TEXT_LAYER_ITEMS) {
    throw new RangeError("PDF text item count exceeds the safety limit");
  }
  const textLayerElement = document.createElement("div");
  textLayerElement.className = "textLayer";
  textLayerElement.setAttribute("aria-label", `第 ${slot.page} 页文本`);
  slot.container.append(textLayerElement);
  const textLayerTask = new TextLayer({
    textContentSource: textContent,
    container: textLayerElement,
    viewport,
  });
  slot.textTask = textLayerTask;
  try {
    await textLayerTask.render();
  } catch (error) {
    if (generation !== documentGeneration || error?.name === "AbortException") return;
    throw error;
  } finally {
    if (slot.textTask === textLayerTask) slot.textTask = null;
  }
  if (generation !== documentGeneration) return;

  slot.placeholder?.remove();
  slot.placeholder = null;
  slot.container.dataset.rendered = "true";
  slot.state = "rendered";
  renderedPages.add(slot.page);
  page.cleanup();
  publishRenderedPages();
  bridge()?.pageRendered?.(slot.page);
  schedulePageSync();
}

function releaseSlot(slot) {
  if (slot.state !== "rendered") return;
  resetSlot(slot);
  publishRenderedPages();
}

/// Keeps the reading window small: pages far from the page being read go back
/// to placeholders, and the page that owns an open note draft is never unloaded.
function scheduleRelease() {
  const locked = bridgeLockedPage();
  const anchor = locked || currentPage;
  if (!anchor) return;
  for (const page of [...renderedPages]) {
    if (page === locked || Math.abs(page - anchor) <= KEEP_RENDERED_RADIUS) continue;
    const slot = slotsByPage.get(page);
    if (!slot || slot.visible) continue;
    releaseSlot(slot);
  }
}

/// The rendered page that covers the viewport centre; the fallback is the
/// rendered page closest to the centre, so a tall neighbouring page still wins.
function dominantPage() {
  const center = window.innerHeight / 2;
  let visible = 0;
  let covered = 0;
  let nearest = 0;
  let nearestDistance = Infinity;
  for (const page of renderedPages) {
    const slot = slotsByPage.get(page);
    if (!slot) continue;
    const rect = slot.container.getBoundingClientRect();
    const overlap = Math.min(rect.bottom, window.innerHeight) - Math.max(rect.top, 0);
    if (overlap > 0) {
      if (rect.top <= center && rect.bottom >= center) return page;
      if (overlap > covered) {
        covered = overlap;
        visible = page;
      }
    }
    const distance = rect.bottom <= center ? center - rect.bottom : rect.top - center;
    if (distance < nearestDistance) {
      nearestDistance = distance;
      nearest = page;
    }
  }
  return visible || nearest || currentPage || reportedPage || 1;
}

function schedulePageSync() {
  if (frameHandle) return;
  frameHandle = requestAnimationFrame(() => {
    frameHandle = 0;
    const locked = bridgeLockedPage();
    const page = locked || dominantPage();
    if (page !== currentPage) {
      currentPage = page;
      bridge()?.setCurrentPage?.(page);
    }
    scheduleRelease();
    if (lastStatusPage !== currentPage) {
      lastStatusPage = currentPage;
      status.textContent = openedPdf
        ? `第 ${currentPage} / ${openedPdf.numPages} 页`
        : "等待本地图书数据…";
    }
    clearTimeout(settleTimer);
    // While a thought is still unsaved the host must stay on the draft page,
    // so scrolling is allowed but never reported as a page change.
    if (locked) return;
    settleTimer = setTimeout(() => {
      settleTimer = 0;
      reportCurrentPage();
    }, PAGE_CHANGE_SETTLE_MS);
  });
}

function reportCurrentPage() {
  if (!openedPdf || bridgeLockedPage()) return;
  const page = clampPage(dominantPage());
  if (!page || page === reportedPage) {
    notifySelectionChanged(true);
    return;
  }
  reportedPage = page;
  notify({
    type: "ngy-pdf-page-changed",
    requestId,
    pageNumber: page,
    pageCount: openedPdf.numPages,
  });
  notifySelectionChanged(true);
}

async function scrollToPage(pageNumber, smooth = true) {
  const page = clampPage(pageNumber);
  const slot = slotsByPage.get(page);
  if (!slot) return;
  try {
    await resolveSize(slot);
  } catch {
    // A page whose size cannot be resolved still gets a best-effort jump.
  }
  requestRender(page, true);
  const top = slot.container.getBoundingClientRect().top + window.scrollY - PAGE_TOP_PADDING;
  window.scrollTo({ top: Math.max(0, top), behavior: smooth ? "smooth" : "auto" });
  schedulePageSync();
}

function reportPageZoom() {
  zoomReportTimer = 0;
  if (!openedPdf) return;
  notify({ type: "ngy-pdf-zoom-changed", requestId, zoomMilli: pageZoomMilli });
}

/// Re-lays the reading column out at one scale.
///
/// The page the reader is looking at stays where it is, so a scale change never
/// moves the reading position; the host is only told about a scale the reader
/// chose, never about one it pushed here itself.
function applyPageZoom(zoomMilli, { report = true } = {}) {
  const next = clampPageZoom(zoomMilli);
  if (next === pageZoomMilli) return;
  const anchor = readingAnchor();
  pageZoomMilli = next;
  applyScaleToSlots();
  restoreReadingAnchor(anchor);
  schedulePageSync();
  if (!report) return;
  clearTimeout(zoomReportTimer);
  zoomReportTimer = setTimeout(reportPageZoom, PAGE_ZOOM_REPORT_MS);
}

async function cancelActive() {
  documentGeneration += 1;
  clearTimeout(settleTimer);
  settleTimer = 0;
  clearTimeout(zoomReportTimer);
  zoomReportTimer = 0;
  if (frameHandle) {
    cancelAnimationFrame(frameHandle);
    frameHandle = 0;
  }
  renderQueue.length = 0;
  for (const slot of slots) {
    slot.task?.cancel();
    slot.textTask?.cancel();
  }
  if (pageObserver) pageObserver.disconnect();
  slots = [];
  slotsByPage = new Map();
  renderedPages.clear();
  pages.replaceChildren();
  currentPage = 0;
  reportedPage = 0;
  lastStatusPage = 0;
  lastSelectionPayload = null;
  viewportEstimate = null;
  if (activeLoadingTask) {
    await activeLoadingTask.destroy();
    activeLoadingTask = null;
  }
  openedPdf = null;
  publishRenderedPages();
}

async function openPdf(request) {
  await cancelActive();
  const generation = documentGeneration;
  const data = bytesFromHost(request.data);
  const startPage = Math.max(1, Number(request.startPage) || 1);
  requestId = Number.isSafeInteger(request.requestId) && request.requestId >= 0
    ? request.requestId
    : 0;
  status.textContent = "正在解析本地 PDF…";
  activeLoadingTask = getDocument({
    data,
    cMapUrl: "./cmaps/",
    cMapPacked: true,
    standardFontDataUrl: "./standard_fonts/",
    wasmUrl: "./wasm/",
    isEvalSupported: false,
    enableXfa: false,
    disableAutoFetch: true,
    disableRange: true,
    disableStream: true,
    useWorkerFetch: false,
    useSystemFonts: true,
    useWasm: true,
    stopEvent: true,
  });
  const opened = await activeLoadingTask.promise;
  if (generation !== documentGeneration) return;
  // The page-count guard runs before any of the document's state is adopted, so
  // a rejected document leaves nothing behind for the zoom and page-step paths
  // to act on: no open document, and no worker still holding its pages. The
  // loading task is the only thing this rejection has created yet, so it is the
  // only thing to release here.
  if (opened.numPages > MAX_RENDER_PAGES) {
    await activeLoadingTask.destroy();
    activeLoadingTask = null;
    throw new RangeError("PDF page count exceeds the safety limit");
  }
  openedPdf = opened;
  notify({
    type: "ngy-pdf-ready",
    requestId,
    pageCount: openedPdf.numPages,
  });
  await seedViewportEstimate();
  if (generation !== documentGeneration) return;
  createSlots(openedPdf.numPages);
  currentPage = clampPage(startPage);
  bridge()?.setCurrentPage?.(currentPage);
  await scrollToPage(currentPage, false);
  schedulePageSync();
}

window.addEventListener("message", (event) => {
  if (event.source !== window || event.origin !== location.origin) return;
  const request = event.data;
  if (!request) return;
  if (request.type === "ngy-pdf-zoom") {
    // The host owns the stored scale; a scale it pushed is not news back to it.
    applyPageZoom(request.zoomMilli, { report: false });
    return;
  }
  if (request.type !== "ngy-pdf-go-to") return;
  if (Number.isSafeInteger(request.requestId) && request.requestId >= 0) {
    requestId = request.requestId;
  }
  scrollToPage(request.pageNumber).catch(reportError);
});

// Ctrl + wheel changes the page scale. The page being read stays where it is
// and the column re-lays out under it, so the notch must not also scroll the
// document. Plain wheel, and every other modifier, keep scrolling.
window.addEventListener(
  "wheel",
  (event) => {
    if (!event.ctrlKey || event.altKey || event.metaKey || event.shiftKey) return;
    if (event.deltaY === 0 || !openedPdf) return;
    event.preventDefault();
    const step = event.deltaY < 0 ? PAGE_ZOOM_STEP_MILLI : -PAGE_ZOOM_STEP_MILLI;
    applyPageZoom(pageZoomMilli + step);
  },
  { passive: false },
);

// Ctrl + Left/Up steps to the previous page and Ctrl + Right/Down to the next.
// The host owns page state, reading progress and note re-binding, so the shell
// only asks for a relative step instead of moving on its own. Plain arrows,
// space, PageUp/PageDown and Home/End keep their native continuous scrolling.
window.addEventListener(
  "keydown",
  (event) => {
    if (!event.ctrlKey || event.altKey || event.metaKey || event.shiftKey) return;
    let delta;
    switch (event.key) {
      case "ArrowLeft":
      case "ArrowUp":
        delta = -1;
        break;
      case "ArrowRight":
      case "ArrowDown":
        delta = 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    const target = delta < 0 ? currentPage - 1 : currentPage + 1;
    if (target < 1 || target > (openedPdf?.numPages ?? 0)) return;
    notify({ type: "ngy-pdf-request-page", delta });
  },
  true,
);

document.addEventListener("scroll", () => schedulePageSync(), { capture: true, passive: true });
window.addEventListener("resize", () => schedulePageSync(), { passive: true });

pageObserver = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      const page = Number(entry.target.dataset.page);
      const slot = slotsByPage.get(page);
      if (!slot) continue;
      slot.visible = entry.isIntersecting;
      if (!entry.isIntersecting) continue;
      if (!slot.sizeKnown) resolveSize(slot).catch(reportError);
      requestRender(page);
    }
    schedulePageSync();
  },
  { root: null, rootMargin: RENDER_ROOT_MARGIN, threshold: 0 },
);

async function openLocalDocument() {
  notify({ type: "ngy-pdf-viewer-ready" });
  const response = await fetch(new URL("./document.pdf", location.href), {
    cache: "no-store",
    credentials: "omit",
    redirect: "error",
    referrerPolicy: "no-referrer",
  });
  if (!response.ok) throw new Error(`unable to read local PDF (${response.status})`);
  const declaredLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(declaredLength) && declaredLength > MAX_PDF_BYTES) {
    throw new RangeError("PDF byte length is outside the safety limit");
  }
  const data = await response.arrayBuffer();
  const startPage = Number(new URLSearchParams(location.search).get("startPage")) || 1;
  await openPdf({ data, startPage, requestId: 0 });
}

openLocalDocument().catch(reportError);

window.addEventListener(
  "pagehide",
  () => {
    clearTimeout(settleTimer);
    clearTimeout(zoomReportTimer);
    if (frameHandle) cancelAnimationFrame(frameHandle);
    frameHandle = 0;
    renderQueue.length = 0;
    pageObserver?.disconnect();
    openedPdf = null;
  },
  { once: true },
);
