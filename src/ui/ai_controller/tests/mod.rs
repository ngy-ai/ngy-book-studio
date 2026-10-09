//! Shared fixtures for the AI controller tests.

pub(crate) use crate::ui::ai_sidebar::{AiBookOption, AiSidebarEvent, AiSidebarScope};

use super::*;
use gpui_kit::component::Root;
use gpui_kit::{AppContext, TestAppContext};
use ngy_book_studio::document::{
    Block, BlockDocument, BookDocument, ContentUnit, ContentUnitKind, DocumentLocator, Revision,
};
use ngy_book_studio::{
    ai::agent::AgentCitationSourceKind,
    ai::chat::{ChatScope, ChatThread, NewChatMessage, NewChatThread, StoredChatMessage},
    library::LibraryStore,
};
use std::{cell::RefCell, rc::Rc, time::Duration};

pub(crate) fn reference(book_id: &str, unit_id: &str, index: usize) -> AiReferenceHint {
    AiReferenceHint::chapter(book_id, unit_id, index, format!("Unit {index}"))
}

mod delete_turn_test;

mod explanation_test;

mod reference_test;
