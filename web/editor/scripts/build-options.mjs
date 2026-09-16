import { fileURLToPath } from "node:url";

const projectDirectory = fileURLToPath(new URL("../", import.meta.url));

export const outputFile = fileURLToPath(
  new URL("../../../assets/editor/prosemirror.js", import.meta.url),
);

export const buildOptions = {
  absWorkingDir: projectDirectory,
  entryPoints: ["src/index.ts"],
  bundle: true,
  // Wry injects initialization scripts at document creation time, before an
  // XHTML document necessarily has a `documentElement`. ProseMirror's bundled
  // dependencies perform browser feature detection while the bundle itself is
  // evaluated, so deferring only our `boot()` function is too late. The outer
  // guard also ensures the privileged bundle is never evaluated for the
  // untrusted content origin used by Preview and rich-text bootstrap fetches.
  banner: {
    js: '(()=>{const __ngyEditorTrustedShell=()=>{try{const url=new URL(window.location.href);return(url.protocol==="epubeditor:"&&url.hostname==="shell")||(url.protocol==="http:"&&url.hostname==="epubeditor.shell")}catch{return false}};const __ngyEditorStart=()=>{if(!__ngyEditorTrustedShell())return;',
  },
  footer: {
    js: '};if(document.readyState==="loading"){document.addEventListener("DOMContentLoaded",__ngyEditorStart,{once:true})}else{__ngyEditorStart()}})();',
  },
  charset: "utf8",
  format: "iife",
  legalComments: "none",
  minify: true,
  platform: "browser",
  sourcemap: false,
  target: ["chrome120"],
};
