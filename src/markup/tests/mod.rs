//! Shared fixtures for the markup tests.

mod block_test;

mod head_test;

mod html_test;

mod projection_test;

use super::*;

pub(super) fn xhtml_body(fragment: &str) -> String {
    format!("<body xmlns=\"{XHTML_NAMESPACE}\">{fragment}</body>")
}
