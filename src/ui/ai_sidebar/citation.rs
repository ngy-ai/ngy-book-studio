//! The DOM script that highlights a cited passage in the reader.

use super::*;

/// Generates a host-owned script that selects one unambiguous DOM text range.
/// It never falls back to the start of the page when the text cannot be mapped.
/// The caller supplies an IPC envelope containing its own page identity.
pub(crate) fn citation_dom_navigation_script(
    focus: &AiCitationTextFocus,
    result_envelope: serde_json::Value,
) -> std::result::Result<String, String> {
    #[derive(Serialize)]
    struct Payload<'a> {
        focus: &'a AiCitationTextFocus,
        result: serde_json::Value,
    }

    let payload = serde_json::to_string(&Payload {
        focus,
        result: result_envelope,
    })
    .map_err(|error| format!("无法编码引用定位请求：{error}"))?;
    Ok(format!(
        r#"(() => {{
  "use strict";
  const payload = {payload};
  const finish = (found, reason) => {{
    try {{
      window.ipc?.postMessage(JSON.stringify({{ ...payload.result, found, reason }}));
    }} catch {{}}
  }};
  const run = () => {{
    const root = document.querySelector('.ProseMirror') || document.body;
    if (!root) {{ finish(false, 'missing_root'); return; }}
    const chars = [];
    const positions = [];
    let pendingSpace = null;
    let hasPendingSpace = false;
    const emitSpace = (position) => {{
      if (chars.length > 0 && !hasPendingSpace) {{
        pendingSpace = position;
        hasPendingSpace = true;
      }}
    }};
    const emitText = (node) => {{
      const value = node.data || '';
      for (let index = 0; index < value.length; index += 1) {{
        const character = value[index];
        if (/\s/u.test(character)) {{
          emitSpace([node, index, index + 1]);
          continue;
        }}
        if (hasPendingSpace) {{
          chars.push(' ');
          positions.push(pendingSpace);
          pendingSpace = null;
          hasPendingSpace = false;
        }}
        chars.push(character);
        positions.push([node, index, index + 1]);
      }}
    }};
    const blockTags = new Set(['ADDRESS','ARTICLE','ASIDE','BLOCKQUOTE','DIV','DL','FIELDSET','FIGCAPTION','FIGURE','FOOTER','FORM','H1','H2','H3','H4','H5','H6','HEADER','HR','LI','MAIN','NAV','OL','P','PRE','SECTION','TABLE','TD','TH','TR','UL']);
    const excludedTags = new Set(['HEAD','SCRIPT','STYLE','NOSCRIPT','TEMPLATE']);
    const walk = (node) => {{
      if (node.nodeType === Node.TEXT_NODE) {{ emitText(node); return; }}
      if (node.nodeType !== Node.ELEMENT_NODE) return;
      if (excludedTags.has(node.tagName)) return;
      if (node.tagName === 'BR') {{ emitSpace(null); return; }}
      const block = blockTags.has(node.tagName);
      if (block) emitSpace(null);
      for (const child of node.childNodes) walk(child);
      if (block) emitSpace(null);
    }};
    walk(root);
    const normalize = (value) => String(value || '').replace(/\s+/gu, ' ').trim();
    const text = normalize(payload.focus.text);
    if (!text) {{ finish(false, 'empty_text'); return; }}
    const context = normalize(`${{payload.focus.before}}${{payload.focus.text}}${{payload.focus.after}}`);
    const haystack = chars.join('');
    let matchStart = haystack.indexOf(context);
    if (matchStart < 0 || haystack.indexOf(context, matchStart + 1) >= 0) {{
      finish(false, matchStart < 0 ? 'not_found' : 'ambiguous');
      return;
    }}
    const throughFocus = normalize(`${{payload.focus.before}}${{payload.focus.text}}`);
    const focusStart = matchStart + Math.max(0, throughFocus.length - text.length);
    const focusEnd = focusStart + text.length;
    const concrete = positions.slice(focusStart, focusEnd).filter(Boolean);
    if (concrete.length === 0) {{ finish(false, 'not_found'); return; }}
    const first = concrete[0];
    const last = concrete[concrete.length - 1];
    try {{
      const range = document.createRange();
      range.setStart(first[0], first[1]);
      range.setEnd(last[0], last[2]);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      (first[0].parentElement || root).scrollIntoView({{ block: 'center', inline: 'nearest' }});
      finish(true, '');
    }} catch {{
      finish(false, 'invalid_range');
    }}
  }};
  if (document.readyState === 'loading') {{
    document.addEventListener('DOMContentLoaded', run, {{ once: true }});
  }} else {{
    run();
  }}
}})();"#
    ))
}
