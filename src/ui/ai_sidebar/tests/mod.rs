//! Shared fixtures for the sidebar tests.

mod citation_test;

mod explanation_test;

mod message_test;

mod request_test;

mod scope_test;

mod stream_test;

use super::*;
use gpui::{AppContext, TestAppContext};
use gpui_component::Root;
use ngy_book_studio::document::{
    Block, BlockDocument, ContentUnit, ContentUnitKind, NormalizedRect,
};
use std::{cell::RefCell, rc::Rc};

fn book(id: &str) -> AiBookOption {
    AiBookOption::new(id, format!("Book {id}"))
}

fn explanation_reference(displayed: Option<&str>) -> AiReferenceHint {
    let mut reference = AiReferenceHint::chapter("book", "unit", 0, "当前章节高亮 · One");
    reference.frozen_text = Some("original passage".to_string());
    reference.displayed_text = displayed.map(str::to_string);
    reference
}
