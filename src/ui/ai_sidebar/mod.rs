use super::*;
use gpui::{EventEmitter, Pixels, rems};
use gpui_component::{
    clipboard::Clipboard,
    text::{TextView, TextViewStyle},
};
use ngy_book_studio::{
    ai::agent::AgentAnswerSourceStatus,
    ai::normalize_provider_base_url,
    document::{BookDocument, ContentUnit, DocumentLocator, Revision, SourceLocator},
    services::{ModelRole, ProviderSettings},
};
use serde::Serialize;
use std::net::IpAddr;

pub(super) mod markdown;
#[cfg(test)]
mod markdown_ui_test;

mod app;

mod citation;

mod command;

mod conversation;

mod event;

mod layout;

mod message;

mod options;

mod reference;

mod render;

mod render_message;

mod render_sidebar;

mod scope;

mod session;

mod session_ops;

pub(crate) use citation::*;

pub(crate) use conversation::*;

pub(crate) use event::*;

pub(crate) use layout::*;

pub(crate) use message::*;

pub(crate) use options::*;

pub(crate) use reference::*;

pub(crate) use scope::*;

pub(crate) use session::*;

#[cfg(test)]
mod tests;
pub(crate) struct AiSidebar {
    conversation: ConversationState,
    sessions: SessionPickerState,
    selection_explanation_request: Option<u64>,
    pending_scope_sync: bool,
    layout: ResponsiveSidebar,
    expanded_width: Pixels,
    endpoint_status: AiEndpointStatus,
    input: Entity<InputState>,
    _endpoint_refresh_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AiSidebarEvent> for AiSidebar {}
impl EventEmitter<AiAnswerCompleted> for AiSidebar {}
impl EventEmitter<AiExplanationSubmitted> for AiSidebar {}
impl EventEmitter<AiExplanationFailed> for AiSidebar {}
