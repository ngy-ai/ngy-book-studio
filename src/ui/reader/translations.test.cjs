// Optional DOM regression gate: node --test src/ui/reader/translations.test.cjs
// Uses an already installed Playwright; never installs dependencies or opens user data.
const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const translationsSource = fs.readFileSync(path.join(__dirname, "translations.js"), "utf8");
const annotationsSource = fs.readFileSync(path.join(__dirname, "annotations.js"), "utf8");
const readerSource = fs.readFileSync(path.join(__dirname, "..", "reader.rs"), "utf8");
const selectionBridgeMatch = readerSource.match(/const READER_INITIALIZATION_SCRIPT: &str = r#"([\s\S]*?)"#;/);
assert.ok(selectionBridgeMatch, "The DOM gate must load the product's actual reader selection bridge");
const selectionBridge = selectionBridgeMatch[1];

// Real chapters are XHTML with the namespace declared, and the gate loads this
// same fixture as `application/xhtml+xml`: without it the elements would belong
// to no namespace and stop being HTML elements at all.
const fixture = `<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><style>
  body{font:22px/1.8 Arial;margin:50px;max-width:760px}p{margin:15px 0}
</style></head><body>
<h1 id="title">无译文标题</h1>
<p id="a">Alpha</p>
<p id="b">Alpha</p>
<p id="c">Beta</p>
<ul><li id="li"><p id="nested">Nested item</p></li></ul>
<blockquote id="quote">Quoted</blockquote>
<table><tbody><tr><td id="cell">Cell</td></tr></tbody></table>
<p id="after">Tail paragraph</p>
</body></html>`;

const payload = {
  session: "translation-session",
  revision: 1,
  blocks: [
    { key: "a", source: "Alpha", segments: [{ source: "Alpha", translated: "甲一" }] },
    { key: "b", source: "Alpha", segments: [{ source: "Alpha", translated: "甲二" }] },
    { key: "c", source: "Beta", segments: [{ source: "Beta", translated: "乙" }] },
    { key: "nested", source: "Nested item", segments: [{ source: "Nested item", translated: "嵌套" }] },
    { key: "quote", source: "Quoted", segments: [{ source: "Quoted", translated: "引用" }] },
    { key: "cell", source: "Cell", segments: [{ source: "Cell", translated: "单元格" }] },
    { key: "after", source: "Tail paragraph", segments: [{ source: "Tail paragraph", translated: "尾段" }] },
  ],
};

let browser;
before(async () => {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.MOYE_TEST_CHROMIUM ? { executablePath: process.env.MOYE_TEST_CHROMIUM } : {}),
  });
});
after(async () => { await browser?.close(); });

async function pageWithFixture(html = fixture, contentType = "text/html; charset=utf-8") {
  const page = await browser.newPage({ viewport: { width: 1150, height: 900 } });
  await page.route("http://epubreader.book/**", (route) => route.fulfill({
    status: 200, contentType, body: html,
    headers: { "content-security-policy": "default-src 'none';script-src 'none';style-src 'unsafe-inline'" },
  }));
  await page.addInitScript({ content: `
    window.__messages = [];
    window.ipc = { postMessage: (body) => window.__messages.push(JSON.parse(body)) };
    const originalAttach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function(options) {
      const result = originalAttach.call(this, options);
      if (this.localName === 'moye-reader-notes') window.__notesRoot = result;
      return result;
    };
    ${selectionBridge}
    ${annotationsSource}
    ${translationsSource}
  ` });
  await page.goto("http://epubreader.book/chapter.html");
  await page.evaluate(() => window.moyeAnnotations.configure({ session: "chapter-session", revision: 1, notes: [] }));
  await page.waitForFunction(() => !!window.__notesRoot);
  return page;
}

async function configure(page) {
  await page.evaluate((value) => window.moyeTranslations.configure(value), payload);
}

function translationText(page, id) {
  return page.evaluate((id) => {
    const node = document.getElementById(id).previousElementSibling;
    return node && node.hasAttribute("data-moye-translation")
      ? node.querySelector(".moye-translation-text").textContent
      : null;
  }, id);
}

test("translated blocks render bilingual above each matched paragraph", async () => {
  const page = await pageWithFixture();
  try {
    await configure(page);
    const state = await page.evaluate(() => {
      const titleSibling = document.getElementById("title").previousElementSibling;
      return {
        count: document.querySelectorAll("[data-moye-translation]").length,
        titleHasMark: !!(titleSibling && titleSibling.hasAttribute("data-moye-translation")),
        listItemTranslations: document.getElementById("li").querySelectorAll("[data-moye-translation]").length,
        cellFirstIsMark: !!document.getElementById("cell").firstElementChild
          && document.getElementById("cell").firstElementChild.hasAttribute("data-moye-translation"),
      };
    });
    assert.equal(state.count, 7, "one translation layer per matched block");
    assert.equal(await translationText(page, "a"), "甲一");
    assert.equal(await translationText(page, "b"), "甲二", "duplicate source text is disambiguated by document order");
    assert.equal(await translationText(page, "c"), "乙");
    assert.equal(await translationText(page, "nested"), "嵌套", "the inner paragraph, not its list item, is translated");
    assert.equal(await translationText(page, "quote"), "引用");
    assert.equal(await translationText(page, "after"), "尾段");
    assert.equal(state.titleHasMark, false, "blocks without a translation keep the original only");
    assert.equal(state.listItemTranslations, 1, "the nested paragraph creates exactly one translation inside its list item");
    assert.equal(state.cellFirstIsMark, true, "table cells receive the translation inside the cell");
  } finally { await page.close(); }
});

test("clicking a translation toggles only that paragraph between bilingual and translation-only", async () => {
  const page = await pageWithFixture();
  try {
    await configure(page);
    const point = await page.evaluate(() => {
      const text = document.getElementById("a").previousElementSibling
        .querySelector(".moye-translation-text");
      const rect = text.getBoundingClientRect();
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
    });
    await page.mouse.click(point.x, point.y);
    const collapsed = await page.evaluate(() => ({
      display: document.getElementById("a").style.display,
      divider: document.getElementById("a").previousElementSibling
        .querySelector(".moye-translation-divider").style.display,
      collapsed: document.getElementById("a").previousElementSibling
        .getAttribute("data-moye-collapsed"),
    }));
    assert.equal(collapsed.display, "none", "translation-only mode hides the original");
    assert.equal(collapsed.divider, "none");
    assert.equal(collapsed.collapsed, "1");

    await page.mouse.click(point.x, point.y);
    const restored = await page.evaluate(() => ({
      display: document.getElementById("a").style.display,
      divider: document.getElementById("a").previousElementSibling
        .querySelector(".moye-translation-divider").style.display,
      neighbour: document.getElementById("b").style.display,
    }));
    assert.equal(restored.display, "", "bilingual mode restores the original");
    assert.notEqual(restored.divider, "none");
    assert.equal(restored.neighbour, "", "the neighbouring paragraph is unaffected");
  } finally { await page.close(); }
});

test("the original-only mode clears the layer and brings the hidden text back", async () => {
  const page = await pageWithFixture();
  try {
    // The reading window pushes an empty payload for "原文": it must remove the
    // layer of the previous mode and restore every hidden original.
    await page.evaluate((value) => window.moyeTranslations.configure(value), {
      ...payload, session: "translation-only-session", revision: 1, displayMode: "translation-only",
    });
    const collapsed = await page.evaluate(() => ({
      applied: window.moyeTranslations.applied(),
      paragraph: document.getElementById("a").style.display,
      // The real list marker must stay visible; the list original is wrapped.
      listItem: getComputedStyle(document.getElementById("li")).display,
      listContent: document.getElementById("li").lastElementChild.style.display,
    }));
    assert.ok(collapsed.applied > 0);
    assert.equal(collapsed.paragraph, "none", "the default mode hides the original paragraph");
    assert.equal(collapsed.listItem, "list-item", "the list marker itself stays visible");
    assert.equal(collapsed.listContent, "none");

    await page.evaluate(() => window.moyeTranslations.configure({
      session: "original-only-session", revision: 1, displayMode: "original-only", blocks: [],
    }));
    const state = await page.evaluate(() => ({
      applied: window.moyeTranslations.applied(),
      blocks: document.querySelectorAll("[data-moye-translation]").length,
      hidden: [...document.querySelectorAll("body *")]
        .filter((node) => node.style.display === "none").length,
      paragraphDisplay: document.getElementById("a").style.display,
      paragraphText: document.getElementById("a").textContent,
      listItemDisplay: getComputedStyle(document.getElementById("li")).display,
      listChildren: [...document.getElementById("li").children].map((node) => node.localName),
      nestedDisplay: getComputedStyle(document.getElementById("nested")).display,
      listText: document.getElementById("li").textContent,
      nestedText: document.getElementById("nested").textContent,
    }));
    assert.equal(state.applied, 0);
    assert.equal(state.blocks, 0);
    assert.equal(state.hidden, 0, "no original may stay hidden once the layer is cleared");
    assert.equal(state.paragraphDisplay, "", "the original paragraph is visible again");
    assert.equal(state.paragraphText, "Alpha");
    assert.equal(state.listItemDisplay, "list-item");
    assert.deepEqual(state.listChildren, ["p"], "the list original is unwrapped again");
    assert.notEqual(state.nestedDisplay, "none");
    assert.equal(state.listText, "Nested item");
    assert.equal(state.nestedText, "Nested item");
  } finally { await page.close(); }
});

test("translation nodes never become book text and selections on them resolve to the originals", async () => {
  const page = await pageWithFixture();
  try {
    await configure(page);

    // A selection spanning two originals must exclude the translation between them.
    await page.evaluate(() => {
      const first = document.getElementById("a");
      const last = document.getElementById("b");
      const range = document.createRange();
      range.setStart(first.firstChild, 0);
      range.setEnd(last.firstChild, last.firstChild.length);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
    });
    await page.waitForTimeout(160);
    const reported = await page.evaluate(() =>
      window.__messages.filter((message) => message.type === "selection_changed").at(-1)?.selected_text);
    assert.equal(reported, "Alpha Alpha", "the selection bridge excludes translation text and keeps paragraph whitespace");

    // The notes toolbar anchors the same range to original book text only.
    const noteQuote = await page.evaluate(async () => {
      const first = document.getElementById("a");
      const last = document.getElementById("b");
      const range = document.createRange();
      range.setStart(first.firstChild, 0);
      range.setEnd(last.firstChild, last.firstChild.length);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
      const toolbar = window.__notesRoot.querySelector(".toolbar");
      for (let i = 0; i < 50 && toolbar.hidden; i++) {
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      if (toolbar.hidden) return null;
      window.__notesRoot.querySelector('[data-action="highlight"]').click();
      return new Promise((resolve) => setTimeout(() => {
        const message = window.__messages.filter((item) => item.action === "highlight").at(-1);
        resolve(message ? message.anchor.quote : null);
      }, 80));
    });
    assert.equal(noteQuote, "Alpha Alpha", "annotation anchors exclude translation text");

    // A selection made on a translation stands for the original it was
    // translated from, both for the chapter reference and for note anchors. The
    // bridge debounces its report, so wait for the message this selection sends.
    const translated = await page.evaluate(async () => {
      const text = document.getElementById("a").previousElementSibling
        .querySelector(".moye-translation-text");
      const before = window.__messages
        .filter((item) => item.type === "selection_changed").length;
      window.getSelection().removeAllRanges();
      const range = document.createRange();
      range.selectNodeContents(text);
      window.getSelection().addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
      const toolbar = window.__notesRoot.querySelector(".toolbar");
      let reported;
      for (let i = 0; i < 50; i += 1) {
        await new Promise((resolve) => setTimeout(resolve, 20));
        if (toolbar.hidden) continue;
        const messages = window.__messages
          .filter((item) => item.type === "selection_changed");
        if (messages.length > before) { reported = messages.at(-1).selected_text; break; }
      }
      return { visible: !toolbar.hidden, selected: window.getSelection().toString(), reported };
    });
    assert.equal(translated.selected, "甲一", "the reader selected the translated text");
    assert.equal(translated.visible, true, "a translated selection is annotatable");
    assert.equal(translated.reported, "Alpha", "the chapter reference quotes the original");

    // Two translated paragraphs resolve to both originals in document order.
    const spanning = await page.evaluate(async () => {
      const first = document.getElementById("a").previousElementSibling
        .querySelector(".moye-translation-text");
      const last = document.getElementById("b").previousElementSibling
        .querySelector(".moye-translation-text");
      const before = window.__messages
        .filter((item) => item.type === "selection_changed").length;
      const range = document.createRange();
      range.setStart(first.firstChild, 0);
      range.setEnd(last.firstChild, last.firstChild.length);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
      for (let i = 0; i < 50; i += 1) {
        await new Promise((resolve) => setTimeout(resolve, 20));
        const messages = window.__messages
          .filter((item) => item.type === "selection_changed");
        if (messages.length > before) return messages.at(-1).selected_text;
      }
      return undefined;
    });
    assert.equal(spanning, "Alpha Alpha", "a translated span resolves to both originals");

    // The native menu reports the text the user saw, which is the translation;
    // the frozen anchor is still the original range it was translated from.
    const explained = await page.evaluate(() => {
      const layer = document.getElementById("a").previousElementSibling;
      const text = layer.querySelector(".moye-translation-text");
      window.getSelection().removeAllRanges();
      const range = document.createRange();
      range.selectNodeContents(text);
      window.getSelection().addRange(range);
      layer.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
      const accepted = window.moyeAnnotations.explainSelection(text.textContent);
      const message = window.__messages.filter((item) => item.action === "ai_explain").at(-1);
      return {
        accepted,
        quote: message?.anchor?.quote ?? null,
        length: message ? message.anchor.end - message.anchor.start : null,
        displayed: message?.displayed_text ?? null,
        hasDisplayed: message ? Object.hasOwn(message, "displayed_text") : false,
      };
    });
    assert.equal(explained.accepted, true, "the native menu accepts a translated selection");
    assert.equal(explained.quote, "Alpha", "the explanation references the original passage");
    assert.equal(explained.length, 5, "the anchor is the whole original leaf, not the translation");
    assert.equal(explained.displayed, "甲一", "the model is told the译文 the reader selected");
    assert.equal(explained.hasDisplayed, true);

    // The floating selection menu takes the same path.
    const viaToolbar = await page.evaluate(async () => {
      const layer = document.getElementById("b").previousElementSibling;
      const text = layer.querySelector(".moye-translation-text");
      window.getSelection().removeAllRanges();
      const range = document.createRange();
      range.selectNodeContents(text);
      window.getSelection().addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
      const toolbar = window.__notesRoot.querySelector(".toolbar");
      for (let i = 0; i < 50 && toolbar.hidden; i += 1) {
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      window.__notesRoot.querySelector('[data-action="ai_explain"]').click();
      const message = window.__messages.filter((item) => item.action === "ai_explain").at(-1);
      return { quote: message?.anchor?.quote ?? null, displayed: message?.displayed_text ?? null };
    });
    assert.deepEqual(viaToolbar, { quote: "Alpha", displayed: "甲二" },
      "the floating menu sends the译文 with the original anchor");

    // Explaining original book text is unchanged: nothing extra is sent.
    const plain = await page.evaluate(() => {
      const paragraph = document.getElementById("a");
      const range = document.createRange();
      range.selectNodeContents(paragraph);
      window.getSelection().removeAllRanges();
      window.getSelection().addRange(range);
      paragraph.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
      const accepted = window.moyeAnnotations.explainSelection("Alpha");
      const message = window.__messages.filter((item) => item.action === "ai_explain").at(-1);
      return { accepted, hasDisplayed: Object.hasOwn(message, "displayed_text") };
    });
    assert.equal(plain.accepted, true);
    assert.equal(plain.hasDisplayed, false, "an original selection sends no译文");
  } finally { await page.close(); }
});

const formattedFixture = `<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><style>
body{font:18px/1.6 Georgia;margin:24px;max-width:850px}
#heading{font-size:38px;font-weight:800;color:rgb(13,65,99);text-align:center}
#heading em{font-size:31px;color:rgb(81,14,113)}
#rich{text-indent:31px;text-align:right;margin-left:17px;line-height:32px;color:rgb(23,45,67)}
#rich strong{font-weight:900}#rich em{font-style:italic}#rich .tone{color:rgb(164,23,78)}
#rich code{font:15px monospace;background-color:rgb(223,231,239)}
#quote{border-left:7px solid rgb(77,88,99);padding-left:19px;margin-left:27px;font-style:italic}
#numbered{list-style-type:upper-roman}td,th{border:2px solid black;padding:12px}
#cell-strong{font-weight:900;color:rgb(22,120,45)}
</style></head><body>
<h2 id="heading">Styled <em>heading</em></h2>
<p id="rich">Alpha <strong>bold</strong><br /><em>italic</em> <del>deleted</del> <sub>down</sub><sup>up</sup> <code id="code-node">a &lt; b</code> <a href="https://invalid.example/link" onclick="window.__unsafe=1">link</a><span class="tone">color</span></p>
<blockquote id="quote">Quote <strong>text</strong></blockquote>
<ol id="numbered" start="5"><li id="one" value="7">First <em>item</em></li><li id="two">Second</li></ol>
<ul id="bulleted"><li id="bullet">Bullet <strong>item</strong></li></ul>
<table><tbody><tr><th id="header" colspan="2">Header</th></tr><tr><td id="table-cell"><strong id="cell-strong">Cell</strong> body</td><td>Untranslated</td></tr></tbody></table>
<p id="media">Look <img id="original-image" alt="Book illustration" src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='10'/%3E" /> image</p>
<p id="after-list">After list</p>
</body></html>`;

const segmentBlock = (key, source, pairs) => ({
  key, source, segments: pairs.map(([source, translated]) => ({ source, translated })),
});
const formattedPayload = {
  session: "formatted-session",
  displayMode: "translation-only",
  blocks: [
    segmentBlock("heading", "Styled heading", [["Styled ", "格式"], ["heading", "标题"]]),
    segmentBlock("rich", "Alpha bolditalic deleted downup a < b linkcolor", [
      ["Alpha ", "正文"], ["bold", "粗体"], ["italic", "斜体"], ["deleted", "删除"],
      ["down", "下标"], ["up", "上标"], ["link", '<img src="https://invalid.example/evil" onerror="window.__unsafe=1" />'],
      ["color", "彩色"],
    ]),
    segmentBlock("quote", "Quote text", [["Quote ", "引用"], ["text", "文字"]]),
    segmentBlock("one", "First item", [["First ", "第一"], ["item", "项"]]),
    segmentBlock("two", "Second", [["Second", "第二项"]]),
    segmentBlock("bullet", "Bullet item", [["Bullet ", "无序"], ["item", "项"]]),
    segmentBlock("header", "Header", [["Header", "表头"]]),
    segmentBlock("table-cell", "Cell body", [["Cell", "单元格"], [" body", "内容"]]),
    segmentBlock("media", "Look image", [["Look ", "查看"], [" image", "插图"]]),
    segmentBlock("after-list", "After list", [["After list", "列表之后"]]),
  ],
};

for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: source formatting, list markers, cells and code survive structured translation safely`, async () => {
    const page = await pageWithFixture(formattedFixture, contentType);
    try {
      const before = await page.evaluate(() => {
        window.__originalLiNodes = Array.from(document.getElementById("one").childNodes);
        window.__originalImage = document.getElementById("original-image");
        const result = {};
        for (const [name, selector, properties] of [
          ["heading", "#heading", ["fontSize", "fontWeight", "color", "textAlign"]],
          ["headingEm", "#heading em", ["fontSize", "fontStyle", "color"]],
          ["rich", "#rich", ["textAlign", "textIndent", "marginLeft", "lineHeight", "color"]],
          ["strong", "#rich strong", ["fontWeight"]],
          ["em", "#rich em", ["fontStyle"]],
          ["del", "#rich del", ["textDecorationLine"]],
          ["sub", "#rich sub", ["verticalAlign", "fontSize"]],
          ["sup", "#rich sup", ["verticalAlign", "fontSize"]],
          ["code", "#rich code", ["fontFamily", "fontSize", "backgroundColor"]],
          ["tone", "#rich .tone", ["color"]],
          ["quote", "#quote", ["borderLeftWidth", "borderLeftStyle", "paddingLeft", "marginLeft"]],
          ["cellStrong", "#cell-strong", ["fontWeight", "color"]],
        ]) {
          const style = getComputedStyle(document.querySelector(selector));
          result[name] = Object.fromEntries(properties.map((property) => [property, style[property]]));
        }
        return result;
      });
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), formattedPayload);
      const state = await page.evaluate((expected) => {
        const translation = (id) => {
          const original = document.getElementById(id);
          const layer = ["li", "td", "th"].includes(original.localName)
            ? original.firstElementChild : original.previousElementSibling;
          return layer.querySelector(".moye-translation-text");
        };
        const nodes = {
          heading: translation("heading"), headingEm: translation("heading").querySelector("em"),
          rich: translation("rich"), strong: translation("rich").querySelector("strong"),
          em: translation("rich").querySelector("em"), del: translation("rich").querySelector("del"),
          sub: translation("rich").querySelector("sub"), sup: translation("rich").querySelector("sup"),
          code: translation("rich").querySelector("code"), tone: translation("rich").lastElementChild,
          quote: translation("quote"), cellStrong: translation("table-cell").querySelector("strong"),
        };
        const styles = Object.fromEntries(Object.entries(nodes).map(([name, element]) => {
          const style = getComputedStyle(element);
          return [name, Object.fromEntries(Object.keys(expected[name]).map((property) => [property, style[property]]))];
        }));
        const rich = translation("rich");
        return {
          styles, applied: window.moyeTranslations.applied(), headingTag: nodes.heading.localName,
          headingNamespace: nodes.heading.namespaceURI,
          headingText: nodes.heading.textContent, boldText: nodes.strong.textContent,
          brCount: rich.querySelectorAll("br").length, code: nodes.code.textContent,
          text: rich.textContent, active: rich.querySelectorAll("a,img,script,iframe,[id],[href],[onclick],[onerror]").length,
          liCount: document.querySelectorAll("li").length,
          olChildren: Array.from(document.getElementById("numbered").children, (node) => node.localName),
          olStart: document.getElementById("numbered").start, liValue: document.getElementById("one").value,
          liDisplay: getComputedStyle(document.getElementById("one")).display,
          liMarker: getComputedStyle(document.getElementById("one")).listStyleType,
          liTranslatedText: translation("one").textContent,
          liOriginalHidden: document.getElementById("one").lastElementChild.style.display,
          liOriginalIdentity: window.__originalLiNodes.every((node, index) =>
            document.getElementById("one").lastElementChild.childNodes[index] === node),
          headerColumns: document.getElementById("header").colSpan,
          cellDisplay: getComputedStyle(document.getElementById("table-cell")).display,
          cellText: translation("table-cell").textContent,
          cellCollapsed: translation("table-cell").parentElement.getAttribute("data-moye-collapsed"),
          mediaVisible: getComputedStyle(document.getElementById("media")).display !== "none",
          imageIdentity: document.getElementById("original-image") === window.__originalImage,
          mediaCopies: document.querySelectorAll("img").length,
        };
      }, before);
      assert.deepEqual(state.styles, before, "computed formatting survives ID-specific source styles and inheritance");
      assert.equal(state.applied, 10);
      assert.equal(state.headingTag, "h2");
      assert.equal(state.headingNamespace, "http://www.w3.org/1999/xhtml");
      assert.equal(state.headingText, "格式 标题");
      assert.equal(state.boldText, "粗体");
      assert.equal(state.brCount, 1);
      assert.equal(state.code, "a < b", "code text remains exactly as authored");
      assert.ok(state.text.includes('<img src="https://invalid.example/evil"'), "model markup is visible plain text");
      assert.equal(state.active, 0, "no source links, source IDs or model HTML become active nodes");
      assert.equal(state.liCount, 3);
      assert.deepEqual(state.olChildren, ["li", "li"], "no div is inserted directly into the list");
      assert.equal(state.olStart, 5);
      assert.equal(state.liValue, 7);
      assert.equal(state.liDisplay, "list-item", "translation-only mode preserves the real list marker");
      assert.equal(state.liMarker, "upper-roman");
      assert.equal(state.liTranslatedText, "第一 项");
      assert.equal(state.liOriginalHidden, "none");
      assert.equal(state.liOriginalIdentity, true);
      assert.equal(state.headerColumns, 2);
      assert.equal(state.cellDisplay, "table-cell");
      assert.equal(state.cellText, "单元格 内容");
      assert.equal(state.cellCollapsed, "0", "table cells stay bilingual");
      assert.equal(state.mediaVisible, true, "translation-only preference never hides the original illustration");
      assert.equal(state.imageIdentity, true);
      assert.equal(state.mediaCopies, 1);

      const restored = await page.evaluate(() => {
        window.moyeTranslations.clear();
        return {
          count: document.querySelectorAll("[data-moye-translation]").length,
          nodesRestored: window.__originalLiNodes.every((node, index) => document.getElementById("one").childNodes[index] === node),
          paragraphStyle: document.getElementById("rich").getAttribute("style"),
          liText: document.getElementById("one").textContent,
          imageSame: document.getElementById("original-image") === window.__originalImage,
          sourceLink: document.querySelector("#rich a").getAttribute("href"),
        };
      });
      assert.deepEqual(restored, {
        count: 0, nodesRestored: true, paragraphStyle: null, liText: "First item",
        imageSame: true, sourceLink: "https://invalid.example/link",
      }, "clear restores the original DOM nodes and display state");
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), formattedPayload);
      assert.equal(await page.evaluate(() => window.moyeTranslations.applied()), 10, "clear/reapply does not accumulate wrappers");
    } finally { await page.close(); }
  });

  test(`${mode}: stale, reordered, missing and legacy leaves keep their original paragraphs visible`, async () => {
    const page = await pageWithFixture(`<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Validation</title></head><body><p id="wrong">One <b>two</b></p><p id="missing">Three <i>four</i></p><p id="legacy">Legacy</p><p id="empty">Empty</p><p id="skipped">Visible<script>ignored()</script><style>.ignored{color:red}</style><span> text</span></p><p id="code">Run <code>print("ok")</code> now</p></body></html>`, contentType);
    try {
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), {
        session: "invalid-session", displayMode: "translation-only", blocks: [
          segmentBlock("wrong", "One two", [["two", "二"], ["One", "一"]]),
          segmentBlock("missing", "Three four", [["Three", "三"]]),
          { key: "legacy", source: "Legacy", translated: "旧格式" },
          segmentBlock("empty", "Empty", [["Empty", "  "]]),
          segmentBlock("skipped", "Visible text", [["Visible", "可见"], ["text", "正文"]]),
          segmentBlock("code", 'Run print("ok") now', [["Run", "运行"], ["now", "现在"]]),
        ],
      });
      const result = await page.evaluate(() => ({
        applied: window.moyeTranslations.applied(),
        unchanged: ["wrong", "missing", "legacy", "empty"].every((id) => document.getElementById(id).style.display !== "none"),
        skipped: document.getElementById("skipped").previousElementSibling.textContent,
        code: document.getElementById("code").previousElementSibling.querySelector("code").textContent,
        codeDisplay: document.getElementById("code").style.display,
      }));
      assert.equal(result.applied, 2);
      assert.equal(result.unchanged, true);
      assert.equal(result.skipped, "可见 正文");
      assert.equal(result.code, 'print("ok")');
      assert.equal(result.codeDisplay, "none");
    } finally { await page.close(); }
  });
}


for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: wrapping list originals preserves note offsets and resolves translated selections`, async () => {
    const page = await pageWithFixture('<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Anchors</title><style>body{font:24px/1.8 Arial;margin:30px}</style></head><body><p id="before">Before</p><ol start="4"><li id="list">A <strong>B</strong></li></ol><p id="end">End</p></body></html>', contentType);
    try {
      const readAnchor = async () => {
        await page.evaluate(() => {
          const range = document.createRange();
          range.selectNodeContents(document.getElementById("end"));
          window.getSelection().removeAllRanges();
          window.getSelection().addRange(range);
          document.dispatchEvent(new Event("selectionchange"));
        });
        await page.waitForFunction(() => !window.__notesRoot.querySelector(".toolbar").hidden);
        return page.evaluate(() => {
          window.__notesRoot.querySelector('[data-action="highlight"]').click();
          const message = window.__messages.filter((item) => item.action === "highlight").at(-1);
          window.moyeAnnotations.result({
            session: message.session, revision: message.revision, request_id: message.request_id, ok: true, notes: [],
          });
          return message.anchor;
        });
      };
      const baseline = await readAnchor();
      assert.deepEqual(baseline, { quote: "End", start: 8, end: 11 });
      await page.evaluate((payload) => {
        window.getSelection().removeAllRanges();
        window.moyeTranslations.configure(payload);
      }, {
        session: "list-session", displayMode: "translation-only",
        blocks: [segmentBlock("list", "A B", [["A ", "甲"], ["B", "乙"]])],
      });
      assert.deepEqual(await readAnchor(), baseline, "the original hidden list leaves retain their UTF-16 offsets");
      await page.evaluate(() => {
        window.getSelection().removeAllRanges();
        const text = document.getElementById("list").querySelector(".moye-translation-text");
        const range = document.createRange();
        range.selectNodeContents(text);
        window.getSelection().addRange(range);
        document.dispatchEvent(new Event("selectionchange"));
      });
      await page.waitForTimeout(160);
      const selection = await page.evaluate(() => ({
        reported: window.__messages.filter((item) => item.type === "selection_changed").at(-1)?.selected_text,
        toolbarHidden: window.__notesRoot.querySelector(".toolbar").hidden,
      }));
      assert.deepEqual(selection, { reported: "A B", toolbarHidden: false },
        "a selection on the translated list item resolves to the original list text");
      await page.evaluate(() => {
        window.getSelection().removeAllRanges();
        document.getElementById("list").querySelector(".moye-translation-text").click();
      });
      assert.equal(await page.evaluate(() => document.getElementById("list").lastElementChild.style.display), "contents", "click reveals original list text without hiding its marker");
      await page.evaluate(() => window.moyeTranslations.clear());
      assert.deepEqual(await readAnchor(), baseline, "clear restores list originals without shifting later notes");
    } finally { await page.close(); }
  });
}

for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: code descendants and code-only blocks do not consume repeated prose translations`, async () => {
    const page = await pageWithFixture('<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Code boundaries</title></head><body><pre><code><p id="inside-code">Same</p></code></pre><p id="only-code"><code>Same</code></p><p id="prose-first">Same</p><p id="prose-second">Same</p><ul><li id="code-container">Lead <pre><p id="nested-code">code()</p></pre> tail</li></ul></body></html>', contentType);
    try {
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), {
        session: "code-boundary-session", displayMode: "translation-only", blocks: [
          segmentBlock("first", "Same", [["Same", "第一处正文"]]),
          segmentBlock("second", "Same", [["Same", "第二处正文"]]),
          segmentBlock("container", "Lead code() tail", [["Lead ", "之前"], [" tail", "之后"]]),
        ],
      });
      const state = await page.evaluate(() => ({
        count: window.moyeTranslations.applied(),
        insideCode: document.getElementById("inside-code").style.display,
        codeOnly: document.getElementById("only-code").style.display,
        codeLayerCount: document.querySelectorAll("pre [data-moye-translation],code [data-moye-translation]").length,
        first: document.getElementById("prose-first").previousElementSibling.querySelector(".moye-translation-text")?.textContent,
        second: document.getElementById("prose-second").previousElementSibling.querySelector(".moye-translation-text")?.textContent,
        container: document.getElementById("code-container").firstElementChild.querySelector(".moye-translation-text")?.textContent,
        preservedCode: document.getElementById("code-container").firstElementChild.querySelector("pre")?.textContent,
      }));
      assert.deepEqual(state, {
        count: 3, insideCode: "", codeOnly: "", codeLayerCount: 0,
        first: "第一处正文", second: "第二处正文", container: "之前 code() 之后", preservedCode: "code()",
      }, "candidate and duplicate-source ordering match the host's skipped code subtrees");
    } finally { await page.close(); }
  });
}

for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: invisible-format-only leaves are not translation slots on either side`, async () => {
    // 宿主现场回归：代码用 ZWSP 缩进，ZWSP 不属于 ECMAScript `\s`。旧叶子列表把它
    // 当成片段，模型只能回空白，整块被 `empty_segment_text` 拒绝；该块现在整体是代码，
    // 不再进入翻译，正文块仍保留同一份叶子过滤。
    const page = await pageWithFixture('<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Invisible leaves</title></head><body><p id="code-line"><span id="indent">\u200b\u200b</span><span>const</span> THREE_AND_A_BIT : f32 = 3.4028236;</p><p id="prose"><span id="prose-indent">\u200b\u200b</span><span>甲</span> 乙</p><p id="blank">\u200b\u00ad</p></body></html>', contentType);
    try {
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), {
        session: "invisible-leaf-session", displayMode: "translation-only", blocks: [
          segmentBlock("code-line", "\u200b\u200bconst THREE_AND_A_BIT : f32 = 3.4028236;", [
            ["const", "常量"],
            [" THREE_AND_A_BIT : f32 = 3.4028236;", "三分之一个字节的常量"],
          ]),
          segmentBlock("prose", "\u200b\u200b甲 乙", [["甲", "甲组"], [" 乙", " 乙组"]]),
        ],
      });
      const state = await page.evaluate(() => ({
        applied: window.moyeTranslations.applied(),
        layers: document.querySelectorAll("[data-moye-translation]").length,
        indent: document.getElementById("indent").textContent,
        codeTranslated: document.getElementById("code-line").previousElementSibling
          ?.querySelector(".moye-translation-text")?.textContent,
        proseIndent: document.getElementById("prose-indent").textContent,
        proseTranslated: document.getElementById("prose").previousElementSibling
          ?.querySelector(".moye-translation-text")?.textContent,
      }));
      assert.deepEqual(state, {
        applied: 1, layers: 1,
        indent: "\u200b\u200b", codeTranslated: undefined,
        proseIndent: "\u200b\u200b", proseTranslated: "\u200b\u200b甲组 乙组",
      }, "the invisible indentation keeps its node, code is skipped whole and only visible prose leaves are replaced");
    } finally { await page.close(); }
  });
}

// The reader may replace the model output of one block by hand. These gates drive
// the shipped layer and assert the two halves of that contract: the page only
// describes an edit (the host revalidates it against the stored row), and nothing
// typed is lost to a background republish, a failure or a cancel.
const manualMessages = (page) => page.evaluate(() =>
  window.__messages.filter((item) => item.type === "manual_translation"));

// The control row is chrome inside the layer's shadow root: its labels must not
// join the block's text, so every lookup goes through that boundary.
const clickControl = (page, id, label) => page.evaluate(({ id, label }) => {
  const layer = document.getElementById(id).previousElementSibling;
  const button = [...layer.querySelector("[data-moye-translation-controls]").shadowRoot
    .querySelectorAll("button")].find((element) => element.textContent === label);
  if (!button) throw new Error(`missing control: ${label}`);
  button.click();
}, { id, label });

const manualState = (page, id) => page.evaluate((id) => {
  const book = document.getElementById(id);
  const layer = book.previousElementSibling;
  const row = layer.querySelector("[data-moye-translation-controls]").shadowRoot;
  const editable = layer.querySelector("[contenteditable='true']");
  return {
    translated: layer.querySelector(".moye-translation-text").textContent,
    editing: !!editable,
    editableText: editable?.textContent ?? null,
    focused: editable ? document.activeElement === editable : false,
    manual: !!row.querySelector(".moye-translation-manual"),
    error: row.querySelector(".moye-translation-error")?.textContent ?? null,
    buttons: [...row.querySelectorAll("button")].map((element) => element.textContent),
    bookText: book.textContent,
    bookHidden: book.style.display,
  };
}, id);

test("a manual edit is posted for the displayed block and survives the host's answer", async () => {
  const page = await pageWithFixture();
  try {
    await configure(page);
    assert.deepEqual(await page.evaluate(() => {
      const layer = document.getElementById("c").previousElementSibling;
      const controls = layer.querySelector("[data-moye-translation-controls]");
      const style = getComputedStyle(controls);
      return {
        buttons: [...controls.shadowRoot.querySelectorAll("button")]
          .map((element) => element.textContent),
        faded: style.opacity,
        clickable: style.pointerEvents,
        tabbable: [...document.querySelectorAll("[data-moye-translation-controls]")]
          .flatMap((element) => [...element.shadowRoot.querySelectorAll("button")]).length,
        // Chrome must never join the block's text: a copied translation would
        // carry the button labels.
        layerText: layer.textContent,
      };
    }), { buttons: ["编辑译文"], faded: "0", clickable: "none", tabbable: 7, layerText: "乙" },
    "every applied block offers an edit affordance that stays out of the way until it is used");

    // Revealing happens on hover or on keyboard focus, and settling the row back
    // happens without rebuilding the layer. The row fades in, so the revealed
    // state is waited for instead of read in the same frame as the focus.
    await page.evaluate(() => {
      const layer = document.getElementById("c").previousElementSibling;
      layer.querySelector("[data-moye-translation-controls]").shadowRoot
        .querySelector("button").focus();
    });
    await page.waitForFunction(() => getComputedStyle(
      document.getElementById("c").previousElementSibling
        .querySelector("[data-moye-translation-controls]")).opacity === "1");
    assert.equal(await page.evaluate(() => getComputedStyle(
      document.getElementById("c").previousElementSibling
        .querySelector("[data-moye-translation-controls]")).opacity), "1",
    "tabbing into the row reveals it");

    await clickControl(page, "c", "编辑译文");
    assert.deepEqual(await manualState(page, "c"), {
      translated: "乙", editing: true, editableText: "乙", focused: true,
      manual: false, error: null, buttons: ["保存", "取消"], bookText: "Beta", bookHidden: "",
    }, "editing replaces only the translated leaf and focuses it");

    // A leaf is one line, so Enter cannot split it into a shape the host could
    // not store; real keystrokes must still reach the editable.
    await page.keyboard.press("Enter");
    await page.keyboard.type("！");
    assert.equal((await manualState(page, "c")).editableText, "乙！");

    await clickControl(page, "c", "保存");
    const sent = (await manualMessages(page)).at(-1);
    assert.deepEqual(sent, {
      type: "manual_translation", action: "update", revision: 1, request_id: sent.request_id, key: "c",
      segments: [{ source: "Beta", translated: "乙！" }],
    }, "the page submits the original leaf as the source, never a rewritten one");
    assert.equal((await manualState(page, "c")).buttons.length, 0, "the editor is busy until the host answers");

    await page.evaluate((request_id) => window.moyeTranslations.result({ request_id, ok: true }),
      sent.request_id);
    assert.deepEqual(await manualState(page, "c"), {
      translated: "乙！", editing: false, editableText: null, focused: false,
      manual: true, error: null, buttons: ["编辑译文", "恢复机器译文"], bookText: "Beta", bookHidden: "",
    }, "a saved edit keeps the reader's text, is marked as manual and can be restored");

    await clickControl(page, "c", "恢复机器译文");
    const restore = (await manualMessages(page)).at(-1);
    assert.deepEqual(restore, {
      type: "manual_translation", action: "restore", revision: 1, request_id: restore.request_id,
      key: "c", segments: [],
    });
    await page.evaluate((request_id) => window.moyeTranslations.result({ request_id, ok: true }),
      restore.request_id);
    assert.deepEqual((await manualState(page, "c")).buttons, ["编辑译文"],
      "restoring drops the manual marker until the host republishes the model text");
  } finally { await page.close(); }
});

test("a failed manual edit keeps the typed text, a republish waits and cancel restores it", async () => {
  const page = await pageWithFixture();
  try {
    await configure(page);
    await page.evaluate(() => {
      window.__editedLayer = document.getElementById("c").previousElementSibling;
    });
    await clickControl(page, "c", "编辑译文");
    await page.keyboard.type("组");
    await clickControl(page, "c", "保存");
    const sent = (await manualMessages(page)).at(-1);
    await page.evaluate((request_id) => window.moyeTranslations.result({
      request_id, ok: false, error: "该文本块的译文已过期，请重新打开本章后再修改",
    }), sent.request_id);
    const failed = await manualState(page, "c");
    assert.equal(failed.editableText, "乙组", "a rejected edit is not thrown away");
    assert.equal(failed.error, "该文本块的译文已过期，请重新打开本章后再修改");
    assert.deepEqual(failed.buttons, ["保存", "取消"]);

    // The background poll republishes the same chapter while a translation job
    // advances; it must not delete text the reader is still typing.
    await page.evaluate((value) => window.moyeTranslations.configure(value), {
      session: "translation-session", revision: 1,
      blocks: [{ key: "c", source: "Beta", segments: [{ source: "Beta", translated: "新乙" }] }],
    });
    assert.equal((await manualState(page, "c")).editableText, "乙组");
    assert.equal(await page.evaluate(() =>
      document.getElementById("c").previousElementSibling === window.__editedLayer), true,
      "the layer being edited is not rebuilt underneath the reader");

    // Cancelling restores the pre-edit model text and then applies what waited.
    await clickControl(page, "c", "取消");
    assert.deepEqual(await manualState(page, "c"), {
      translated: "新乙", editing: false, editableText: null, focused: false,
      manual: false, error: null, buttons: ["编辑译文"], bookText: "Beta", bookHidden: "",
    }, "cancel restores the model text and the deferred payload is applied");
    assert.equal(await page.evaluate(() => window.moyeTranslations.applied()), 1,
      "the waiting payload is the one that took effect");
  } finally { await page.close(); }
});

test("an empty manual translation is refused before it leaves the page", async () => {
  const page = await pageWithFixture();
  try {
    await configure(page);
    const before = (await manualMessages(page)).length;
    await clickControl(page, "c", "编辑译文");
    await page.keyboard.press("Control+A");
    await page.keyboard.press("Backspace");
    await clickControl(page, "c", "保存");
    const state = await manualState(page, "c");
    assert.equal(state.editableText, "");
    assert.equal(state.error, "译文不能为空；如需还原请使用「恢复机器译文」");
    assert.deepEqual(state.buttons, ["保存", "取消"]);
    assert.equal((await manualMessages(page)).length, before, "nothing was sent for an empty edit");
    await clickControl(page, "c", "取消");
    assert.equal((await manualState(page, "c")).translated, "乙");
  } finally { await page.close(); }
});

for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: editing a translation keeps the book text and resolves selections to it`, async () => {
    const page = await pageWithFixture(undefined, contentType);
    try {
      await configure(page);
      await clickControl(page, "c", "编辑译文");
      const edited = await page.evaluate(() => {
        const book = document.getElementById("c");
        const editable = book.previousElementSibling.querySelector("[contenteditable='true']");
        const range = document.createRange();
        range.selectNodeContents(editable);
        window.getSelection().removeAllRanges();
        window.getSelection().addRange(range);
        return {
          bookText: book.textContent,
          translatedTo: window.moyeTranslations.originalRange(range)?.toString(),
        };
      });
      assert.deepEqual(edited, { bookText: "Beta", translatedTo: "Beta" },
        "the editable holds presentation only; notes still anchor to the book text");
      await clickControl(page, "c", "取消");
    } finally { await page.close(); }
  });
}

for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: a block already marked manual offers restore and skips the editor`, async () => {
    const page = await pageWithFixture(undefined, contentType);
    try {
      await page.evaluate((value) => window.moyeTranslations.configure(value), {
        session: "manual-session", revision: 2,
        blocks: [
          { key: "a", source: "Alpha", segments: [{ source: "Alpha", translated: "人工甲" }], manual: true },
          segmentBlock("c", "Beta", [["Beta", "乙"]]),
        ],
      });
      const state = await manualState(page, "a");
      assert.equal(state.translated, "人工甲");
      assert.equal(state.manual, true);
      assert.deepEqual(state.buttons, ["编辑译文", "恢复机器译文"]);
      assert.equal((await manualState(page, "c")).manual, false, "the marker is per block");
      await clickControl(page, "a", "编辑译文");
      await page.keyboard.type("改");
      await clickControl(page, "a", "保存");
      const sent = (await manualMessages(page)).at(-1);
      assert.deepEqual(sent.segments, [{ source: "Alpha", translated: "人工甲改" }],
        "editing a manual block sends its stored source, not the displayed text");
    } finally { await page.close(); }
  });
}

for (const contentType of ["text/html; charset=utf-8", "application/xhtml+xml; charset=utf-8"]) {
  const mode = contentType.startsWith("application") ? "XHTML" : "HTML";
  test(`${mode}: code-shaped paragraphs keep their original text`, async () => {
    // 未用 pre/code 标记的代码（Calibre/Word 转换、验收 EPUB 的缩进代码行）也必须
    // 不翻译；正文句子即使带括号、URL 或全角标点仍然翻译。
    const page = await pageWithFixture('<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Code shape</title></head><body><p id="statement">const TOTAL : f32 = 3.5;</p><p id="braces">fn main() {</p><p id="comment">// 注释</p><p id="operator">count =&gt; count + 1</p><p id="prose">Read the note (see above).</p><p id="url">https://example.test/a/b</p></body></html>', contentType);
    try {
      await page.evaluate((payload) => window.moyeTranslations.configure(payload), {
        session: "code-shape-session", displayMode: "translation-only", blocks: [
          segmentBlock("statement", "const TOTAL : f32 = 3.5;", [["const TOTAL : f32 = 3.5;", "常量"]]),
          segmentBlock("braces", "fn main() {", [["fn main() {", "主函数 {"]]),
          segmentBlock("comment", "// 注释", [["// 注释", "注释"]]),
          segmentBlock("operator", "count => count + 1", [["count => count + 1", "计数递增"]]),
          segmentBlock("prose", "Read the note (see above).", [["Read the note (see above).", "读上面的说明。"]]),
          segmentBlock("url", "https://example.test/a/b", [["https://example.test/a/b", "示例链接"]]),
        ],
      });
      const state = await page.evaluate(() => {
        const layered = (id) => !!document.getElementById(id).previousElementSibling
          ?.hasAttribute("data-moye-translation");
        const text = (id) => document.getElementById(id).previousElementSibling
          ?.querySelector(".moye-translation-text")?.textContent;
        return {
          applied: window.moyeTranslations.applied(),
          statement: layered("statement"), braces: layered("braces"),
          comment: layered("comment"), operator: layered("operator"),
          prose: layered("prose"), url: layered("url"),
          proseText: text("prose"), urlText: text("url"),
        };
      });
      assert.deepEqual(state, {
        applied: 2,
        statement: false, braces: false, comment: false, operator: false,
        prose: true, url: true,
        proseText: "读上面的说明。", urlText: "示例链接",
      }, "code keeps its original text while prose and a bare URL stay translatable");
    } finally { await page.close(); }
  });
}
