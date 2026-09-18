//! Publishing the answer events, including the terminal ones on drop.

pub(super) use super::*;

pub(crate) const PROTOCOL_MARKER_START: &str = "[[ngy-";

/// Publishes provisional deltas and guarantees that every non-committed turn
/// which exposed text is followed by a reset, including cancellation and
/// early-error paths reached through `?`.
pub(crate) struct AnswerEventPublisher {
    pub(crate) sender: Option<mpsc::UnboundedSender<AgentRunEvent>>,
    pub(crate) visible: bool,
    pub(crate) settled: bool,
}

impl AnswerEventPublisher {
    pub(super) fn new(sender: Option<&mpsc::UnboundedSender<AgentRunEvent>>) -> Self {
        Self {
            sender: sender.cloned(),
            visible: false,
            settled: false,
        }
    }

    pub(super) fn delta(&mut self, delta: String) {
        if delta.is_empty() {
            return;
        }
        if self
            .sender
            .as_ref()
            .is_some_and(|sender| sender.send(AgentRunEvent::AnswerDelta(delta)).is_ok())
        {
            self.visible = true;
        }
    }

    pub(super) fn reset(&mut self) {
        if self.visible
            && let Some(sender) = self.sender.as_ref()
        {
            let _ = sender.send(AgentRunEvent::AnswerReset);
        }
        self.visible = false;
        self.settled = true;
    }

    pub(super) fn commit(&mut self) {
        if let Some(sender) = self.sender.as_ref() {
            let _ = sender.send(AgentRunEvent::AnswerCommitted);
        }
        self.settled = true;
    }
}

impl Drop for AnswerEventPublisher {
    fn drop(&mut self) {
        if self.visible && !self.settled {
            if let Some(sender) = self.sender.as_ref() {
                let _ = sender.send(AgentRunEvent::AnswerReset);
            }
            self.visible = false;
        }
    }
}
