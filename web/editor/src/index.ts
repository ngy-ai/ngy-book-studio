import {
  chainCommands,
  createParagraphNear,
  deleteSelection,
  exitCode,
  liftEmptyBlock,
  newlineInCode,
  setBlockType,
  splitBlock,
  toggleMark,
  wrapIn,
} from "prosemirror-commands";
import { history, redo, undo } from "prosemirror-history";
import { keymap } from "prosemirror-keymap";
import {
  DOMParser as ProseMirrorDOMParser,
  DOMSerializer,
  type MarkSpec,
  type Node as ProseMirrorNode,
  type NodeSpec,
  Schema,
} from "prosemirror-model";
import { schema as basicSchema } from "prosemirror-schema-basic";
import {
  addListNodes,
  liftListItem,
  sinkListItem,
  splitListItem,
  wrapInList,
} from "prosemirror-schema-list";
import { EditorState, NodeSelection, type Command } from "prosemirror-state";
import {
  addColumnAfter,
  addRowAfter,
  columnResizing,
  deleteColumn,
  deleteRow,
  deleteTable,
  goToNextCell,
  tableNodes,
  tableEditing,
} from "prosemirror-tables";
import { EditorView } from "prosemirror-view";

const XHTML_NAMESPACE = "http://www.w3.org/1999/xhtml";
const MAX_BODY_BYTES = 8 * 1024 * 1024;
const MAX_DOCUMENT_BYTES = MAX_BODY_BYTES + 64 * 1024;
const MAX_RESOURCE_URL_LENGTH = 16 * 1024;
const MAX_LABEL_LENGTH = 4 * 1024;
const MAX_RAW_HTML_LENGTH = 512 * 1024;
const MAX_SELECTED_TEXT_BYTES = 32 * 1024;
const MAX_IPC_ID_BYTES = 4 * 1024;
const SNAPSHOT_DEBOUNCE_MS = 80;

type ResourceKind = "image" | "audio" | "video";

interface ResourceBlock {
  type: ResourceKind;
  src: string;
  alt?: string;
  title?: string;
  poster?: string;
}

interface EditorHostApi {
  readonly version: 1;
  focus(): void;
  insertResource(resource: ResourceBlock): boolean;
  replaceSelectedResource(resource: ResourceBlock): boolean;
  insertTable(rows?: number, columns?: number): boolean;
  insertRestrictedHtml(source: string): boolean;
  deleteSelection(): boolean;
}

declare global {
  interface Window {
    ipc?: { postMessage(message: string): void };
    __ngyEditorInstalled?: boolean;
    __ngyEditorSend?: (requestId?: number) => void;
    __ngyProseMirror?: EditorHostApi;
  }
}

function boundedText(value: unknown, maximum = MAX_LABEL_LENGTH): string {
  return typeof value === "string" ? value.slice(0, maximum) : "";
}

function trustedShellUrl(url: URL): boolean {
  return (
    (url.protocol === "epubeditor:" && url.hostname === "shell") ||
    (url.protocol === "http:" && url.hostname === "epubeditor.shell")
  );
}

function contentDocumentUrl(url: URL): boolean {
  return (
    (url.protocol === "epubeditor:" && url.hostname === "content") ||
    (url.protocol === "http:" && url.hostname === "epubeditor.content")
  );
}

function richSourceUrl(shellUrl: URL): URL | null {
  if (!trustedShellUrl(shellUrl)) return null;
  const source = new URL(shellUrl.href);
  source.hostname =
    source.protocol === "http:" ? "epubeditor.content" : "content";
  source.searchParams.set("mode", "source");
  return source;
}

function uniqueQueryValue(url: URL, key: string): string | null {
  const values = url.searchParams.getAll(key);
  return values.length === 1 ? values[0] : null;
}

function validIpcId(value: string | null): value is string {
  return (
    value !== null &&
    value.length > 0 &&
    value.trim() === value &&
    !/[\u0000-\u001f\u007f]/u.test(value) &&
    new TextEncoder().encode(value).byteLength <= MAX_IPC_ID_BYTES
  );
}

function safeResourceUrl(value: unknown, kind: ResourceKind): string | null {
  if (typeof value !== "string") return null;
  const candidate = value.trim();
  if (!candidate || candidate.length > MAX_RESOURCE_URL_LENGTH) return null;

  const allowedDataPrefix = `data:${kind}/`;
  if (candidate.toLowerCase().startsWith(allowedDataPrefix)) {
    return candidate;
  }

  try {
    const source = richSourceUrl(new URL(window.location.href));
    if (!source || candidate.startsWith("//")) return null;
    const parsed = new URL(candidate, source);
    if (
      contentDocumentUrl(parsed) &&
      !parsed.username &&
      !parsed.password &&
      !parsed.search &&
      !parsed.hash &&
      /^\/\.ngy\/assets\/[A-Za-z0-9._~-]+$/u.test(parsed.pathname)
    ) {
      return parsed.href;
    }
  } catch {
    return null;
  }
  return null;
}

function safeLinkHref(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const candidate = value.trim();
  if (!candidate || candidate.length > MAX_RESOURCE_URL_LENGTH) return null;
  if (candidate.startsWith("#")) return candidate;
  if (/^[./]/.test(candidate) && !candidate.startsWith("//")) return candidate;
  if (!/^[a-z][a-z\d+.-]*:/i.test(candidate)) return candidate;
  try {
    return contentDocumentUrl(new URL(candidate, window.location.href)) ? candidate : null;
  } catch {
    return null;
  }
}

function element(value: Node): HTMLElement | null {
  return value instanceof HTMLElement ? value : null;
}

function resourceAttrs(dom: Node, kind: ResourceKind): false | Record<string, string> {
  const target = element(dom);
  const src = safeResourceUrl(target?.getAttribute("src"), kind);
  if (!target || !src) return false;
  return {
    kind,
    src,
    alt: boundedText(target.getAttribute("alt")),
    title: boundedText(target.getAttribute("title")),
    poster:
      kind === "video"
        ? safeResourceUrl(target.getAttribute("poster"), "image") ?? ""
        : "",
  };
}

const imageSpec: NodeSpec = {
  inline: true,
  attrs: {
    src: {},
    alt: { default: "" },
    title: { default: "" },
  },
  group: "inline",
  draggable: true,
  parseDOM: [
    {
      tag: "img[src]",
      getAttrs(dom) {
        const attrs = resourceAttrs(dom, "image");
        return attrs === false
          ? false
          : { src: attrs.src, alt: attrs.alt, title: attrs.title };
      },
    },
  ],
  toDOM(node) {
    return [
      "img",
      {
        src: safeResourceUrl(node.attrs.src, "image") ?? "",
        alt: boundedText(node.attrs.alt),
        title: boundedText(node.attrs.title),
        loading: "lazy",
        decoding: "async",
      },
    ];
  },
};

const mediaSpec: NodeSpec = {
  group: "block",
  atom: true,
  draggable: true,
  selectable: true,
  attrs: {
    kind: { default: "audio" },
    src: {},
    title: { default: "" },
    poster: { default: "" },
  },
  parseDOM: [
    { tag: "audio[src]", getAttrs: (dom) => resourceAttrs(dom, "audio") },
    { tag: "video[src]", getAttrs: (dom) => resourceAttrs(dom, "video") },
  ],
  toDOM(node) {
    const kind: "audio" | "video" = node.attrs.kind === "video" ? "video" : "audio";
    const attrs: Record<string, string> = {
      src: safeResourceUrl(node.attrs.src, kind) ?? "",
      title: boundedText(node.attrs.title),
      controls: "controls",
      preload: "metadata",
    };
    if (kind === "video") {
      const poster = safeResourceUrl(node.attrs.poster, "image");
      if (poster) attrs.poster = poster;
    }
    return [kind, attrs];
  },
};

// Raw HTML is deliberately represented as source text. It is never inserted as
// live DOM, so script, event-handler and remote-resource markup cannot execute.
const restrictedHtmlSpec: NodeSpec = {
  content: "text*",
  group: "block",
  code: true,
  defining: true,
  marks: "",
  parseDOM: [{ tag: "pre[data-ngy-raw-html]", preserveWhitespace: "full" }],
  toDOM() {
    return ["pre", { "data-ngy-raw-html": "restricted" }, ["code", 0]];
  },
};

const strikeSpec: MarkSpec = {
  parseDOM: [{ tag: "del" }, { tag: "s" }, { style: "text-decoration=line-through" }],
  toDOM() {
    return ["del", 0];
  },
};

let nodes = addListNodes(basicSchema.spec.nodes, "paragraph block*", "block");
nodes = nodes.append(
  tableNodes({
    tableGroup: "block",
    cellContent: "paragraph block*",
    cellAttributes: {},
  }),
);
nodes = nodes.update("image", imageSpec);
nodes = nodes.addBefore("image", "media", mediaSpec);
nodes = nodes.addBefore("code_block", "restricted_html", restrictedHtmlSpec);

const baseLink = basicSchema.spec.marks.get("link");
const linkSpec: MarkSpec = {
  ...baseLink,
  attrs: { href: {}, title: { default: null } },
  inclusive: false,
  parseDOM: [
    {
      tag: "a[href]",
      getAttrs(dom) {
        const target = element(dom);
        const href = safeLinkHref(target?.getAttribute("href"));
        return href
          ? { href, title: boundedText(target?.getAttribute("title")) || null }
          : false;
      },
    },
  ],
  toDOM(node) {
    return [
      "a",
      {
        href: safeLinkHref(node.attrs.href) ?? "#",
        title: boundedText(node.attrs.title) || null,
        rel: "noreferrer",
      },
      0,
    ];
  },
};

let marks = basicSchema.spec.marks.update("link", linkSpec);
marks = marks.addToEnd("strike", strikeSpec);

const schema = new Schema({ nodes, marks });

function decodeEditorHref(pathname: string): string | null {
  try {
    return decodeURIComponent(pathname.replace(/^\/+/, ""));
  } catch {
    return null;
  }
}

/**
 * Reports whether the payload actually reached the host bridge. A missing
 * `window.ipc` and a throwing `postMessage` both mean "not delivered", so a
 * caller holding unsendable state can keep it instead of assuming success.
 * Callers that only announce offline state may ignore the result: the Rust-side
 * readiness timeout still covers a host that never becomes available.
 */
function postIpc(payload: Record<string, unknown>): boolean {
  const bridge = window.ipc;
  if (!bridge) return false;
  try {
    bridge.postMessage(JSON.stringify(payload));
    return true;
  } catch {
    return false;
  }
}

function parseInitialDocument(body: Element): ProseMirrorNode {
  const source = body.cloneNode(true) as HTMLElement;
  source
    .querySelectorAll(
      "script,style,iframe,frame,object,embed,form,input,button,textarea,select,link,meta,base,[data-ngy-editor-ui]",
    )
    .forEach((node) => node.remove());
  return ProseMirrorDOMParser.fromSchema(schema).parse(source, {
    preserveWhitespace: true,
  });
}

async function fetchInitialDocument(shellUrl: URL): Promise<ProseMirrorNode> {
  const sourceUrl = richSourceUrl(shellUrl);
  if (!sourceUrl) throw new Error("untrusted editor shell origin");
  const response = await fetch(sourceUrl, {
    cache: "no-store",
    credentials: "omit",
    redirect: "error",
    referrerPolicy: "no-referrer",
  });
  if (!response.ok || !response.url || new URL(response.url).href !== sourceUrl.href) {
    throw new Error("editor content request was rejected");
  }
  const contentType = response.headers.get("content-type")?.toLowerCase() ?? "";
  if (!contentType.startsWith("application/xhtml+xml")) {
    throw new Error("editor content has an invalid media type");
  }
  const declaredLength = Number(response.headers.get("content-length"));
  if (
    Number.isFinite(declaredLength) &&
    declaredLength > MAX_DOCUMENT_BYTES
  ) {
    throw new Error("editor content exceeds the size limit");
  }
  const source = await response.text();
  if (new TextEncoder().encode(source).byteLength > MAX_DOCUMENT_BYTES) {
    throw new Error("editor content exceeds the size limit");
  }
  const parsed = new DOMParser().parseFromString(source, "application/xhtml+xml");
  if (parsed.getElementsByTagName("parsererror").length !== 0) {
    throw new Error("editor content is not valid XHTML");
  }
  const bodies = parsed.getElementsByTagNameNS(XHTML_NAMESPACE, "body");
  if (bodies.length !== 1) throw new Error("editor content has no unique body");
  return parseInitialDocument(bodies[0]);
}

function makeTable(rows: number, columns: number): ProseMirrorNode | null {
  const table = schema.nodes.table;
  const row = schema.nodes.table_row;
  const cell = schema.nodes.table_cell;
  if (!table || !row || !cell) return null;

  const safeRows = Math.max(1, Math.min(30, Math.trunc(rows)));
  const safeColumns = Math.max(1, Math.min(20, Math.trunc(columns)));
  const tableRows = Array.from({ length: safeRows }, () =>
    row.create(
      null,
      Array.from({ length: safeColumns }, () => cell.createAndFill()).filter(
        (value): value is ProseMirrorNode => value !== null,
      ),
    ),
  );
  return table.create(null, tableRows);
}

function button(label: string, title: string, command: () => boolean): HTMLButtonElement {
  const control = document.createElementNS(XHTML_NAMESPACE, "button") as HTMLButtonElement;
  control.type = "button";
  control.textContent = label;
  control.title = title;
  control.setAttribute("aria-label", title);
  control.addEventListener("mousedown", (event) => event.preventDefault());
  control.addEventListener("click", (event) => {
    event.preventDefault();
    command();
  });
  return control;
}

async function installEditor(): Promise<void> {
  if (window.__ngyEditorInstalled || !document.body) return;

  const url = new URL(window.location.href);
  const revisionSource = uniqueQueryValue(url, "rev");
  const revision = Number(revisionSource);
  const session_id = uniqueQueryValue(url, "session_id");
  const chapter_id = uniqueQueryValue(url, "chapter_id");
  const href = decodeEditorHref(url.pathname);
  if (
    window.top !== window ||
    !trustedShellUrl(url) ||
    uniqueQueryValue(url, "mode") !== "edit" ||
    revisionSource === null ||
    !/^(0|[1-9]\d*)$/u.test(revisionSource) ||
    !Number.isSafeInteger(revision) ||
    revision < 0 ||
    !validIpcId(session_id) ||
    !validIpcId(chapter_id) ||
    href === null
  ) {
    return;
  }

  window.__ngyEditorInstalled = true;
  const initialDocument = await fetchInitialDocument(url);
  document.body.replaceChildren();
  document.body.setAttribute("data-ngy-rich-editor", "true");

  const style = document.createElementNS(XHTML_NAMESPACE, "style") as HTMLStyleElement;
  style.dataset.ngyEditorUi = "style";
  style.textContent = `
    body[data-ngy-rich-editor="true"] { min-height: 100vh; caret-color: #b95f42; }
    [data-ngy-editor-ui="toolbar"] {
      box-sizing: border-box; position: sticky; top: 0; z-index: 2147483647;
      display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin: 0 0 14px;
      padding: 8px; border: 1px solid #ded8cf; border-radius: 9px;
      background: rgba(255, 254, 250, 0.97); box-shadow: 0 4px 14px rgba(41,38,33,.10);
      font: 13px/1.2 "Microsoft YaHei", sans-serif; color: #292621;
    }
    [data-ngy-editor-ui="toolbar"] button {
      box-sizing: border-box; cursor: pointer; padding: 5px 9px;
      border: 1px solid #ded8cf; border-radius: 6px; background: #fffefa;
      font: 13px/1.2 "Microsoft YaHei", sans-serif; color: #292621;
    }
    [data-ngy-editor-ui="toolbar"] button:hover { background: #f1ddd5; border-color: #b95f42; }
    [data-ngy-editor-ui="surface"] .ProseMirror { min-height: 70vh; outline: none; }
    [data-ngy-editor-ui="surface"] .ProseMirror-selectednode { outline: 2px solid #b95f42; }
    [data-ngy-editor-ui="surface"] table { border-collapse: collapse; margin: 1em 0; }
    [data-ngy-editor-ui="surface"] th,
    [data-ngy-editor-ui="surface"] td { border: 1px solid #aaa; min-width: 3em; padding: .35em; }
    [data-ngy-editor-ui="surface"] .column-resize-handle { background: #b95f42; width: 4px; }
    [data-ngy-editor-ui="surface"] pre[data-ngy-raw-html] {
      white-space: pre-wrap; border-left: 3px solid #b95f42; padding: .75em; background: #f7f4ee;
    }
    [data-ngy-editor-ui="surface"] img,
    [data-ngy-editor-ui="surface"] video { max-width: 100%; height: auto; }
    [data-ngy-editor-ui="surface"] audio { width: min(100%, 36em); }
  `;
  (document.head || document.documentElement).appendChild(style);

  const toolbar = document.createElementNS(XHTML_NAMESPACE, "div") as HTMLDivElement;
  toolbar.dataset.ngyEditorUi = "toolbar";
  toolbar.setAttribute("contenteditable", "false");
  toolbar.setAttribute("role", "toolbar");

  const surface = document.createElementNS(XHTML_NAMESPACE, "div") as HTMLDivElement;
  surface.dataset.ngyEditorUi = "surface";
  document.body.append(toolbar, surface);

  let dirty = false;
  let sendTimer = 0;
  let view: EditorView;

  const serializeBody = (): string => {
    const body = document.createElementNS(XHTML_NAMESPACE, "body") as HTMLBodyElement;
    body.setAttribute("xmlns", XHTML_NAMESPACE);
    const language = document.documentElement.getAttribute("lang");
    if (language) body.setAttribute("lang", boundedText(language, 128));
    body.appendChild(
      DOMSerializer.fromSchema(schema).serializeFragment(view.state.doc.content, { document }),
    );
    return new XMLSerializer().serializeToString(body);
  };

  const selectedText = (): string => {
    const { from, to } = view.state.selection;
    const value = view.state.doc.textBetween(from, to, " ", " ").replace(/\s+/gu, " ").trim();
    if (!value) return "";
    const encoder = new TextEncoder();
    if (encoder.encode(value).byteLength <= MAX_SELECTED_TEXT_BYTES) return value;
    let low = 0;
    let high = value.length;
    while (low < high) {
      const middle = Math.ceil((low + high) / 2);
      if (encoder.encode(value.slice(0, middle)).byteLength <= MAX_SELECTED_TEXT_BYTES) {
        low = middle;
      } else {
        high = middle - 1;
      }
    }
    return value.slice(0, low).trim();
  };

  const sendSnapshot = (requestId?: number): void => {
    if (
      requestId !== undefined &&
      (!Number.isSafeInteger(requestId) || requestId <= 0)
    ) {
      return;
    }
    window.clearTimeout(sendTimer);
    const selected_text = selectedText();
    if (!dirty) {
      postIpc({
        session_id,
        chapter_id,
        href,
        revision,
        request_id: Number.isSafeInteger(requestId) ? requestId : null,
        body: null,
        selected_text,
        too_large: false,
      });
      return;
    }

    const serialized = serializeBody();
    if (new TextEncoder().encode(serialized).byteLength > MAX_BODY_BYTES) {
      postIpc({
        session_id,
        chapter_id,
        href,
        revision,
        request_id: Number.isSafeInteger(requestId) ? requestId : null,
        body: null,
        selected_text,
        too_large: true,
      });
      return;
    }
    const delivered = postIpc({
      session_id,
      chapter_id,
      href,
      revision,
      request_id: Number.isSafeInteger(requestId) ? requestId : null,
      body: serialized,
      selected_text,
      too_large: false,
    });
    // Only a send the bridge accepted clears `dirty`. A dropped body keeps the
    // page dirty, so the next debounced edit, blur, or host `__ngyEditorSend`
    // retries this same body under the existing throttle instead of echoing an
    // empty snapshot that would freeze stale Rust state. No timer is re-armed
    // here, so a permanently dead bridge cannot spin.
    if (delivered) {
      dirty = false;
    }
  };

  const queueSnapshot = (): void => {
    window.clearTimeout(sendTimer);
    sendTimer = window.setTimeout(() => sendSnapshot(), SNAPSHOT_DEBOUNCE_MS);
  };

  const run = (command: Command): boolean => {
    const handled = command(view.state, view.dispatch, view);
    if (handled) view.focus();
    return handled;
  };

  const insertNode = (node: ProseMirrorNode): boolean => {
    view.dispatch(view.state.tr.replaceSelectionWith(node).scrollIntoView());
    view.focus();
    return true;
  };

  const resourceNode = (resource: ResourceBlock): ProseMirrorNode | null => {
    const src = safeResourceUrl(resource.src, resource.type);
    if (!src) return null;
    if (resource.type === "image") {
      return schema.nodes.image.create({
        src,
        alt: boundedText(resource.alt),
        title: boundedText(resource.title),
      });
    }
    return schema.nodes.media.create({
      kind: resource.type,
      src,
      title: boundedText(resource.title),
      poster:
        resource.type === "video"
          ? safeResourceUrl(resource.poster, "image") ?? ""
          : "",
    });
  };

  const replaceSelectedResource = (resource: ResourceBlock): boolean => {
    const node = resourceNode(resource);
    const selection = view.state.selection;
    if (!node || !(selection instanceof NodeSelection)) return false;
    const selected = selection.node.type;
    if (selected !== schema.nodes.image && selected !== schema.nodes.media) return false;
    view.dispatch(view.state.tr.replaceSelectionWith(node).scrollIntoView());
    view.focus();
    return true;
  };

  const shortcuts: Record<string, Command> = {
    "Mod-z": undo,
    "Shift-Mod-z": redo,
    "Mod-y": redo,
    "Mod-b": toggleMark(schema.marks.strong),
    "Mod-i": toggleMark(schema.marks.em),
    "Mod-`": toggleMark(schema.marks.code),
    "Shift-Ctrl-8": wrapInList(schema.nodes.bullet_list),
    "Shift-Ctrl-9": wrapInList(schema.nodes.ordered_list),
    Enter: splitListItem(schema.nodes.list_item),
    "Mod-[": liftListItem(schema.nodes.list_item),
    "Mod-]": sinkListItem(schema.nodes.list_item),
    Tab: goToNextCell(1),
    "Shift-Tab": goToNextCell(-1),
  };

  view = new EditorView(surface, {
    state: EditorState.create({
      doc: initialDocument,
      schema,
      plugins: [
        history(),
        keymap(shortcuts),
        keymap({
          Enter: chainCommands(
            newlineInCode,
            createParagraphNear,
            liftEmptyBlock,
            splitBlock,
          ),
          "Mod-Enter": exitCode,
        }),
        keymap({
          Backspace: chainCommands(deleteSelection),
        }),
        columnResizing(),
        tableEditing(),
      ],
    }),
    dispatchTransaction(transaction) {
      const nextState = view.state.apply(transaction);
      view.updateState(nextState);
      if (transaction.docChanged) {
        dirty = true;
      }
      if (transaction.docChanged || transaction.selectionSet) {
        queueSnapshot();
      }
    },
    handleDOMEvents: {
      click(_view, event) {
        const target = event.target;
        if (target instanceof Element && target.closest("a")) {
          event.preventDefault();
          return true;
        }
        return false;
      },
      auxclick(_view, event) {
        if (event.target instanceof Element && event.target.closest("a")) {
          event.preventDefault();
          return true;
        }
        return false;
      },
      drop(_view, event) {
        // Files must enter through the Rust host so their bytes are stored and
        // represented by a book-scoped object URL before they reach the AST.
        if (event.dataTransfer?.files.length) {
          event.preventDefault();
          return true;
        }
        return false;
      },
    },
  });

  const controls: Array<[string, string, Command]> = [
    ["正文", "正文", setBlockType(schema.nodes.paragraph)],
    ["标题 1", "一级标题", setBlockType(schema.nodes.heading, { level: 1 })],
    ["标题 2", "二级标题", setBlockType(schema.nodes.heading, { level: 2 })],
    ["加粗", "加粗", toggleMark(schema.marks.strong)],
    ["斜体", "斜体", toggleMark(schema.marks.em)],
    ["删除线", "删除线", toggleMark(schema.marks.strike)],
    ["项目符号", "项目符号列表", wrapInList(schema.nodes.bullet_list)],
    ["编号", "编号列表", wrapInList(schema.nodes.ordered_list)],
    ["引用", "引用", wrapIn(schema.nodes.blockquote)],
    ["代码", "代码块", setBlockType(schema.nodes.code_block)],
    ["撤销", "撤销", undo],
    ["重做", "重做", redo],
  ];
  for (const [label, title, command] of controls) {
    toolbar.appendChild(button(label, title, () => run(command)));
  }
  toolbar.appendChild(
    button("表格", "插入 3 × 3 表格", () => {
      const table = makeTable(3, 3);
      return table ? insertNode(table) : false;
    }),
  );

  window.__ngyEditorSend = (requestId?: number) => sendSnapshot(requestId);
  window.__ngyProseMirror = Object.freeze({
    version: 1 as const,
    focus: () => view.focus(),
    insertResource(resource: ResourceBlock) {
      const node = resourceNode(resource);
      return node ? insertNode(node) : false;
    },
    replaceSelectedResource,
    insertTable(rows = 3, columns = 3) {
      const table = makeTable(rows, columns);
      return table ? insertNode(table) : false;
    },
    insertRestrictedHtml(source: string) {
      if (typeof source !== "string" || source.length > MAX_RAW_HTML_LENGTH) return false;
      const text = schema.text(source);
      return insertNode(schema.nodes.restricted_html.create(null, text));
    },
    deleteSelection: () => run(deleteSelection),
  });

  document.addEventListener("blur", () => sendSnapshot(), true);
  window.addEventListener("pagehide", () => window.clearTimeout(sendTimer), { once: true });
  view.focus();
  postIpc({
    session_id,
    chapter_id,
    href,
    revision,
    request_id: null,
    body: null,
    selected_text: "",
    too_large: false,
    ready: true,
  });
}

function boot(): void {
  const start = (): void => {
    void installEditor().catch(() => {
      // The Rust-side readiness timeout reports a local content/bootstrap
      // failure without accepting a partial editor session.
    });
  };
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start, { once: true });
  } else {
    start();
  }
}

boot();
