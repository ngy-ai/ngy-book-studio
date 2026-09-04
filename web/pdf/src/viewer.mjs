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
const status = document.querySelector("#status");
const pages = document.querySelector("#pages");
const originalFetch = globalThis.fetch.bind(globalThis);
const utf8Encoder = new TextEncoder();
let activeLoadingTask = null;
let activeRenderTask = null;
let activeTextLayerTask = null;
let activeTextLayerElement = null;
let openedPdf = null;
let renderGeneration = 0;
let renderedRequestId = 0;
let renderedPageNumber = 0;
let lastSelectionPayload = null;

GlobalWorkerOptions.workerSrc = "./pdf.worker.mjs";

// PDF.js only receives bytes from the trusted host. Same-origin fetch remains
// available for bundled CMaps/fonts/Wasm; CSP independently blocks every
// network origin.
globalThis.fetch = (input, init) => {
  const raw = typeof input === "string" ? input : input.url;
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

function selectionInsideCurrentTextLayer(selection) {
  return activeTextLayerElement
    && selection
    && selection.rangeCount === 1
    && !selection.isCollapsed
    && activeTextLayerElement.contains(selection.anchorNode)
    && activeTextLayerElement.contains(selection.focusNode);
}

function notifySelectionChanged(force = false) {
  if (!activeTextLayerElement || renderedPageNumber < 1) return;
  const selection = document.getSelection();
  const selectedText = selectionInsideCurrentTextLayer(selection)
    ? boundedNormalizedSelection(selection.toString())
    : "";
  const payload = JSON.stringify([
    renderedRequestId,
    renderedPageNumber,
    selectedText,
  ]);
  if (!force && payload === lastSelectionPayload) return;
  lastSelectionPayload = payload;
  notify({
    type: "moye-pdf-selection-changed",
    requestId: renderedRequestId,
    pageNumber: renderedPageNumber,
    selectedText,
  });
}

function clearRenderedSelection() {
  const selection = document.getSelection();
  if (selection && selection.rangeCount > 0) selection.removeAllRanges();
  activeTextLayerTask?.cancel();
  activeTextLayerTask = null;
  activeTextLayerElement = null;
  renderedPageNumber = 0;
  lastSelectionPayload = null;
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

async function cancelActive() {
  renderGeneration += 1;
  clearRenderedSelection();
  if (activeRenderTask) {
    activeRenderTask.cancel();
    activeRenderTask = null;
  }
  if (activeLoadingTask) {
    await activeLoadingTask.destroy();
    activeLoadingTask = null;
  }
  openedPdf = null;
}

async function renderPage(pageNumber, requestId) {
  if (!openedPdf) throw new Error("PDF is not ready");
  const generation = ++renderGeneration;
  if (activeRenderTask) {
    activeRenderTask.cancel();
    activeRenderTask = null;
  }
  clearRenderedSelection();
  pages.replaceChildren();
  const selectedPage = Math.min(
    openedPdf.numPages,
    Math.max(1, Number(pageNumber) || 1),
  );
  const page = await openedPdf.getPage(selectedPage);
  const viewport = page.getViewport({ scale: 1.5 });
  const pageContainer = document.createElement("section");
  pageContainer.className = "pdf-page";
  pageContainer.style.width = `${Math.ceil(viewport.width)}px`;
  pageContainer.style.height = `${Math.ceil(viewport.height)}px`;
  pageContainer.style.setProperty("--total-scale-factor", String(viewport.scale));
  pageContainer.dataset.page = String(selectedPage);
  const canvas = document.createElement("canvas");
  canvas.className = "page-canvas";
  canvas.width = Math.ceil(viewport.width);
  canvas.height = Math.ceil(viewport.height);
  canvas.dataset.page = String(selectedPage);
  pageContainer.append(canvas);
  pages.append(pageContainer);
  status.textContent = `正在渲染第 ${selectedPage} 页…`;
  const context = canvas.getContext("2d", { alpha: false });
  activeRenderTask = page.render({
    canvasContext: context,
    viewport,
    annotationMode: AnnotationMode.DISABLE,
    intent: "display",
  });
  try {
    await activeRenderTask.promise;
  } catch (error) {
    if (generation !== renderGeneration || error?.name === "RenderingCancelledException") {
      return;
    }
    throw error;
  } finally {
    if (generation === renderGeneration) activeRenderTask = null;
  }
  if (generation !== renderGeneration) return;

  const textContent = await page.getTextContent({
    includeMarkedContent: true,
    disableNormalization: false,
  });
  if (generation !== renderGeneration) return;
  if (textContent.items.length > MAX_TEXT_LAYER_ITEMS) {
    throw new RangeError("PDF text item count exceeds the safety limit");
  }
  const textLayerElement = document.createElement("div");
  textLayerElement.className = "textLayer";
  textLayerElement.setAttribute("aria-label", `第 ${selectedPage} 页文本`);
  pageContainer.append(textLayerElement);
  activeTextLayerTask = new TextLayer({
    textContentSource: textContent,
    container: textLayerElement,
    viewport,
  });
  try {
    await activeTextLayerTask.render();
  } catch (error) {
    if (generation !== renderGeneration || error?.name === "AbortException") return;
    throw error;
  } finally {
    if (generation === renderGeneration) activeTextLayerTask = null;
  }
  if (generation !== renderGeneration) return;
  activeTextLayerElement = textLayerElement;
  renderedRequestId = requestId;
  renderedPageNumber = selectedPage;
  page.cleanup();
  status.textContent = `${selectedPage} / ${openedPdf.numPages}`;
  notify({
    type: "moye-pdf-page-changed",
    requestId,
    pageNumber: selectedPage,
    pageCount: openedPdf.numPages,
  });
  notifySelectionChanged(true);
}

async function openPdf(request) {
  await cancelActive();
  const data = bytesFromHost(request.data);
  const startPage = Math.max(1, Number(request.startPage) || 1);
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
  openedPdf = await activeLoadingTask.promise;
  if (openedPdf.numPages > MAX_RENDER_PAGES) {
    throw new RangeError("PDF page count exceeds the safety limit");
  }
  notify({
    type: "moye-pdf-ready",
    requestId: request.requestId,
    pageCount: openedPdf.numPages,
  });
  await renderPage(startPage, request.requestId);
}

window.addEventListener("message", (event) => {
  if (event.source !== window || event.origin !== location.origin) return;
  const request = event.data;
  if (!request || request.type !== "moye-pdf-go-to") return;
  renderPage(request.pageNumber, request.requestId).catch((error) => {
    status.textContent = "PDF 预览失败";
    notify({
      type: "moye-pdf-error",
      requestId: request.requestId,
      message: String(error?.message || error).slice(0, 1000),
    });
  });
});

async function openLocalDocument() {
  notify({ type: "moye-pdf-viewer-ready" });
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

openLocalDocument().catch((error) => {
  status.textContent = "PDF 预览失败";
  notify({
    type: "moye-pdf-error",
    requestId: 0,
    message: String(error?.message || error).slice(0, 1000),
  });
});
