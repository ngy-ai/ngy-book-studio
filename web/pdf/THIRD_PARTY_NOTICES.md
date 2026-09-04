# PDF.js third-party notice

The generated files under `assets/pdfjs/` come from `pdfjs-dist` version
5.7.284, published by the Mozilla PDF.js project under Apache-2.0. The upstream
license is copied to `assets/pdfjs/LICENSE.pdfjs` by the build script.

The viewer shell is project-owned code. It accepts PDF bytes only, disables PDF
JavaScript evaluation and XFA, and blocks external network connections. Only
same-origin requests for the checked-in CMaps, fonts, and Wasm are permitted.
