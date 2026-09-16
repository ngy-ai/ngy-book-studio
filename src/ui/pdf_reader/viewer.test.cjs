// Optional DOM regression gate: node --test src/ui/pdf_reader/viewer.test.cjs
// Uses an already installed Playwright; never installs dependencies or opens user data.
//
// The reading shell is a real PDF.js document, so this gate drives the shipped
// assets/pdfjs bundle with a generated PDF: the whole document is one scrollable
// column, pages far from the reading position are released and drawn again on
// return, the shell reports the page it settled on and which page owns a
// selection, and the compact reading preference never moves the reading
// position. Drive it with NGY_TEST_CHROMIUM when no bundled browser exists.
const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const ASSETS = path.join(__dirname, "..", "..", "..", "assets", "pdfjs");
const PAGES = 20;
const VIEWPORT = { width: 1000, height: 900 };
// Page 12 sits at the top of the reading area after a host navigation.
const PAGE_TOP_PADDING = 12;

const CONTENT_TYPES = {
  ".html": "text/html; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".wasm": "application/wasm",
};

/// A uniform multi-page PDF: identical page boxes keep the column geometry
/// predictable, so a layout measurement is never confused with a page that
/// resolved a different size.
function buildPdf(pageCount) {
  const fontId = 2 * pageCount + 3;
  const objects = [];
  objects[1] = "<< /Type /Catalog /Pages 2 0 R >>";
  const kids = [];
  for (let index = 0; index < pageCount; index += 1) kids.push(`${3 + index * 2} 0 R`);
  objects[2] = `<< /Type /Pages /Kids [${kids.join(" ")}] /Count ${pageCount} >>`;
  for (let index = 0; index < pageCount; index += 1) {
    const pageId = 3 + index * 2;
    const contentId = pageId + 1;
    objects[pageId] =
      `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 500] ` +
      `/Resources << /Font << /F1 ${fontId} 0 R >> >> /Contents ${contentId} 0 R >>`;
    const text = `BT /F1 20 Tf 40 440 Td (Page ${index + 1} of ${pageCount}) Tj ET`;
    objects[contentId] = `<< /Length ${text.length + 1} >>\nstream\n${text}\nendstream`;
  }
  objects[fontId] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";
  let output = "%PDF-1.4\n";
  const offsets = [];
  for (let id = 1; id <= fontId; id += 1) {
    offsets[id] = output.length;
    output += `${id} 0 obj\n${objects[id]}\nendobj\n`;
  }
  const startxref = output.length;
  output += `xref\n0 ${fontId + 1}\n0000000000 65535 f \n`;
  for (let id = 1; id <= fontId; id += 1) {
    output += `${String(offsets[id]).padStart(10, "0")} 00000 n \n`;
  }
  output += `trailer\n<< /Size ${fontId + 1} /Root 1 0 R >>\nstartxref\n${startxref}\n%%EOF\n`;
  return Buffer.from(output, "latin1");
}

const pdf = buildPdf(PAGES);

let browser;
before(async () => {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.NGY_TEST_CHROMIUM ? { executablePath: process.env.NGY_TEST_CHROMIUM } : {}),
  });
});
after(async () => { await browser?.close(); });

/// Opens the shipped viewer over the private origin the product uses, serving
/// the checked-in bundle exactly as the host does.
async function openViewer(query = "") {
  const page = await browser.newPage({ viewport: VIEWPORT });
  await page.addInitScript(() => {
    window.__messages = [];
    window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
    // Page-spacing changes are reported through the loops the shell itself uses,
    // so a failing position assertion can show what moved and what corrected it.
    window.__scrollCalls = [];
    const scrollBy = window.scrollBy.bind(window);
    window.scrollBy = (...args) => {
      window.__scrollCalls.push(["by", ...args]);
      return scrollBy(...args);
    };
    const scrollTo = window.scrollTo.bind(window);
    window.scrollTo = (...args) => {
      window.__scrollCalls.push(["to", ...args]);
      return scrollTo(...args);
    };
  });
  await page.route("http://ngypdf.viewer/**", (route) => {
    const name = new URL(route.request().url()).pathname.replace(/^\//, "");
    if (name === "document.pdf") {
      return route.fulfill({ status: 200, contentType: "application/pdf", body: pdf });
    }
    const file = path.join(ASSETS, name);
    if (!file.startsWith(ASSETS) || !fs.existsSync(file)) {
      return route.fulfill({ status: 404, body: "missing" });
    }
    return route.fulfill({
      status: 200,
      contentType: CONTENT_TYPES[path.extname(file)] || "application/octet-stream",
      body: fs.readFileSync(file),
    });
  });
  await page.goto(`http://ngypdf.viewer/viewer.html${query}`);
  await waitForMessage(page, "ngy-pdf-ready");
  return page;
}

async function waitForMessage(page, type, predicate = null) {
  await page.waitForFunction(
    ({ type, predicate }) => window.__messages.some((message) => {
      if (message.type !== type) return false;
      if (!predicate) return true;
      return Object.entries(predicate).every(([key, value]) => message[key] === value);
    }),
    { type, predicate },
    { timeout: 20000 },
  );
}

async function lastMessage(page, type) {
  return page.evaluate((type) => window.__messages.filter((m) => m.type === type).at(-1), type);
}

/// Everything a layout assertion needs, in one round trip.
async function snapshot(page) {
  return page.evaluate(() => {
    const center = window.innerHeight / 2;
    const slots = [...document.querySelectorAll(".pdf-page")];
    const anchor = slots.find((slot) => {
      const rect = slot.getBoundingClientRect();
      return rect.top <= center && rect.bottom >= center;
    });
    return {
      height: document.documentElement.scrollHeight,
      scrollY: window.scrollY,
      slotCount: slots.length,
      gap: slots.length ? parseFloat(getComputedStyle(slots[0]).marginBottom) : NaN,
      compact: document.documentElement.getAttribute("data-pdf-compact"),
      calls: window.__scrollCalls.slice(),
      anchor: anchor
        ? {
            page: Number(anchor.dataset.page),
            top: anchor.getBoundingClientRect().top,
            rendered: anchor.dataset.rendered,
          }
        : null,
    };
  });
}

/// Waits for the reading position and the column geometry to stop moving, so a
/// measurement is never taken while pages are still being drawn.
async function settle(page) {
  let previous = null;
  for (let attempt = 0; attempt < 15; attempt += 1) {
    const current = await snapshot(page);
    if (
      previous &&
      previous.height === current.height &&
      previous.anchor &&
      current.anchor &&
      previous.anchor.page === current.anchor.page &&
      Math.abs(previous.anchor.top - current.anchor.top) < 0.5
    ) {
      return current;
    }
    previous = current;
    await page.waitForTimeout(120);
  }
  return previous;
}

async function selectPageText(page, pageNumber) {
  return page.evaluate((pageNumber) => {
    const layer = document.querySelector(`.pdf-page[data-page="${pageNumber}"] .textLayer`);
    const range = document.createRange();
    range.selectNodeContents(layer);
    const selection = document.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
    return selection.toString();
  }, pageNumber);
}

test("the whole document is one scrollable column that reports the page it settled on", async () => {
  const page = await openViewer();
  try {
    const ready = await lastMessage(page, "ngy-pdf-ready");
    assert.equal(ready.pageCount, PAGES);

    const column = await page.evaluate(() => {
      const slots = [...document.querySelectorAll(".pdf-page")];
      const tops = slots.map((slot) => slot.getBoundingClientRect().top + window.scrollY);
      return {
        pages: slots.map((slot) => Number(slot.dataset.page)),
        stacked: tops.every((top, index) => index === 0 || top > tops[index - 1]),
        height: document.documentElement.scrollHeight,
      };
    });
    assert.deepEqual(
      column.pages,
      Array.from({ length: PAGES }, (_, index) => index + 1),
      "every page is laid out at once instead of one page at a time",
    );
    assert.ok(column.stacked, "pages are stacked downwards in one column");
    assert.ok(column.height > VIEWPORT.height * 3, "the column is scrollable");

    // Plain scrolling reports the page the reader settled on, plus the total.
    await page.evaluate(() => window.scrollTo(0, 2400));
    await waitForMessage(page, "ngy-pdf-page-changed", { pageNumber: 4, pageCount: PAGES });
    const changed = await lastMessage(page, "ngy-pdf-page-changed");
    assert.equal(changed.requestId, 0, "plain scrolling is not a host navigation");

    // Ctrl + Arrow asks the host for a relative step instead of moving alone.
    await page.keyboard.press("Control+ArrowDown");
    await waitForMessage(page, "ngy-pdf-request-page", { delta: 1 });
    assert.equal(await page.evaluate(() => window.scrollY), 2400, "the shell only asks");

    // A host navigation scrolls to the page and reuses its request id.
    await page.evaluate(() =>
      window.postMessage({ type: "ngy-pdf-go-to", requestId: 9, pageNumber: 12 }, location.origin));
    await waitForMessage(page, "ngy-pdf-page-changed", { requestId: 9, pageNumber: 12 });
    const top = await page.evaluate(() =>
      document.querySelector('.pdf-page[data-page="12"]').getBoundingClientRect().top);
    assert.ok(
      Math.abs(top - PAGE_TOP_PADDING) <= 2,
      `the requested page starts the reading area (top=${top})`,
    );
  } finally { await page.close(); }
});

test("pages far from the reading position are released and drawn again on return", async () => {
  const page = await openViewer();
  try {
    await page.waitForFunction(
      () => document.querySelector('.pdf-page[data-page="1"]').dataset.rendered === "true",
      null,
      { timeout: 20000 },
    );
    const height = await page.evaluate(() => document.documentElement.scrollHeight);
    await page.evaluate((height) => window.scrollTo(0, height), height);
    await waitForMessage(page, "ngy-pdf-page-changed", { pageNumber: PAGES });
    await page.waitForFunction(
      () => document.querySelector('.pdf-page[data-page="1"]').dataset.rendered === "false",
      null,
      { timeout: 20000 },
    );
    const released = await page.evaluate(() => {
      const slot = document.querySelector('.pdf-page[data-page="1"]');
      return {
        placeholder: slot.querySelector(".page-placeholder")?.textContent || "",
        height: slot.getBoundingClientRect().height,
      };
    });
    assert.equal(released.placeholder, "第 1 页", "a released page keeps its page number");
    assert.equal(
      released.height,
      await page.evaluate(() =>
        document.querySelector('.pdf-page[data-page="2"]').getBoundingClientRect().height),
      "releasing a page keeps the column geometry",
    );

    // The page being read is never released, and coming back draws it again
    // without keeping its old placeholder.
    const anchor = await settle(page);
    assert.equal(anchor.anchor.rendered, "true", "the page being read stays drawn");
    await page.evaluate(() => window.scrollTo(0, 0));
    await waitForMessage(page, "ngy-pdf-page-changed", { pageNumber: 1 });
    await page.waitForFunction(
      () => document.querySelector('.pdf-page[data-page="1"]').dataset.rendered === "true",
      null,
      { timeout: 20000 },
    );
    assert.equal(
      await page.evaluate(() =>
        !!document.querySelector('.pdf-page[data-page="1"] .page-placeholder')),
      false,
      "the returned page is drawn again instead of showing its placeholder",
    );
  } finally { await page.close(); }
});

test("a text selection is reported with the page that owns it", async () => {
  const page = await openViewer();
  try {
    await page.evaluate(() => window.scrollTo(0, 2400));
    await waitForMessage(page, "ngy-pdf-page-changed", { pageNumber: 4 });
    const selected = await selectPageText(page, 4);
    assert.ok(selected.includes("Page 4"), `the page text layer is selectable (${selected})`);
    await page.waitForFunction(
      () => window.__messages.some((message) =>
        message.type === "ngy-pdf-selection-changed" && message.pageNumber === 4 &&
        message.selectedText.includes("Page 4")),
      null,
      { timeout: 20000 },
    );

    // Clearing the selection keeps the page the reader is on, with no text.
    await page.evaluate(() => {
      document.getSelection().removeAllRanges();
      document.dispatchEvent(new Event("selectionchange"));
    });
    await page.waitForFunction(
      () => window.__messages.some((message) =>
        message.type === "ngy-pdf-selection-changed" && message.pageNumber === 4 &&
        message.selectedText === ""),
      null,
      { timeout: 20000 },
    );
  } finally { await page.close(); }
});

test("compact reading applies from the URL and toggling it keeps the reading position", async () => {
  // The URL parameter is enough: that document never has a page gap at all.
  const compact = await openViewer("?startPage=3&compact=1");
  try {
    const first = await snapshot(compact);
    assert.equal(first.compact, "1", "compact=1 names the attribute");
    assert.equal(first.gap, 0, "compact=1 removes the gap before the first slot is drawn");
    assert.equal(first.slotCount, PAGES);
    await waitForMessage(compact, "ngy-pdf-page-changed", { pageNumber: 3 });
  } finally { await compact.close(); }

  // Toggling it on a scrolled document is a pure page-spacing change: only the
  // page boxes shrink, and the page being read stays where the reader sees it.
  const page = await openViewer();
  try {
    await page.evaluate(() => window.scrollTo(0, 6000));
    const before = await settle(page);
    assert.ok(before.gap > 0, "the default reading keeps a page gap");
    assert.ok(before.anchor, "a page carries the reading position");

    await page.evaluate(() => document.documentElement.setAttribute("data-pdf-compact", "1"));
    const after = await settle(page);
    assert.equal(after.gap, 0, "compact reading removes the gap");
    assert.equal(
      before.height - after.height,
      before.gap * PAGES,
      "only the page gaps disappear from the column",
    );
    assert.equal(after.anchor.page, before.anchor.page, "the same page stays under the reader");
    assert.ok(
      Math.abs(after.anchor.top - before.anchor.top) <= 1,
      `compact reading keeps the reading position (before=${JSON.stringify(before)} after=${JSON.stringify(after)})`,
    );

    const beforeRestore = await settle(page);
    await page.evaluate(() => document.documentElement.removeAttribute("data-pdf-compact"));
    const afterRestore = await settle(page);
    assert.equal(afterRestore.gap, before.gap, "restoring the preference restores the gap");
    assert.equal(
      afterRestore.height - beforeRestore.height,
      before.gap * PAGES,
      "restoring the gap only adds the page gaps back",
    );
    assert.ok(
      afterRestore.anchor.page === beforeRestore.anchor.page &&
        Math.abs(afterRestore.anchor.top - beforeRestore.anchor.top) <= 1,
      `restoring the gap keeps the reading position (before=${JSON.stringify(beforeRestore)} after=${JSON.stringify(afterRestore)})`,
    );
  } finally { await page.close(); }
});

/// Ctrl + wheel re-lays the reading column out at a new page scale: the page
/// being read keeps its place inside that page, only the scale the reader chose
/// is reported back, and a scale the host pushed is not news back to the host.
test("ctrl + wheel changes the page scale without moving the reading position", async () => {
  const page = await openViewer();
  try {
    const pageWidth = () => page.evaluate(() =>
      document.querySelector(".pdf-page").getBoundingClientRect().width);
    // The viewport starts 200px into page 12, so the anchor page's top sits well
    // above the window and a scale change has to move it by a measurable amount.
    await page.evaluate(() => {
      const slot = document.querySelector('.pdf-page[data-page="12"]');
      window.scrollTo(0, slot.getBoundingClientRect().top + window.scrollY + 200);
    });
    const before = await settle(page);
    const widthBefore = await pageWidth();
    assert.equal(before.anchor?.page, 12, "the fixture must be read on page 12");
    assert.ok(before.anchor.top < -50, `page 12 must start above the window: ${before.anchor.top}`);

    await page.mouse.move(VIEWPORT.width / 2, VIEWPORT.height / 2);
    await page.keyboard.down("Control");
    await page.mouse.wheel(0, -120);
    await page.keyboard.up("Control");

    await waitForMessage(page, "ngy-pdf-zoom-changed", { zoomMilli: 1750 });
    const after = await settle(page);
    assert.ok(await pageWidth() > widthBefore, `page width must grow: ${widthBefore}`);

    // The anchor keeps its place inside its own page, which is what a reader
    // looking at a point in the text expects a scale change to preserve.
    assert.equal(after.anchor?.page, 12, "the page being read must not change");
    const expected = before.anchor.top * (1750 / 1500);
    assert.ok(
      Math.abs(after.anchor.top - expected) < 12,
      `page 12 top ${before.anchor.top} -> ${after.anchor.top}, expected about ${expected}`,
    );

    // A scale the host pushes is applied but never reported back as a change.
    const reported = () => page.evaluate(() =>
      window.__messages.filter((message) => message.type === "ngy-pdf-zoom-changed").length);
    const reportsBefore = await reported();
    await page.evaluate(() => window.postMessage(
      { type: "ngy-pdf-zoom", zoomMilli: 1000 },
      window.location.origin,
    ));
    const shrunk = await settle(page);
    assert.ok(await pageWidth() < widthBefore, "the pushed scale must re-lay the column out");
    assert.equal(shrunk.anchor?.page, 12);
    await page.waitForTimeout(500);
    assert.equal(await reported(), reportsBefore,
      "the host's own scale must not come back as a change");
  } finally {
    await page.close();
  }
});
