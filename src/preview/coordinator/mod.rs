//! The coordinator that claims visual jobs and drives them to completion.

use super::*;

mod blocking;

mod coordinator;

mod execute;

mod jobs;

mod util;

mod validate;

pub(crate) use blocking::*;

pub(crate) use execute::*;

pub(crate) use jobs::*;

pub(crate) use util::*;

pub(crate) use validate::*;
pub struct VisualJobCoordinator {
    pub(crate) core: Arc<CoordinatorCore>,
    pub(crate) sender: async_channel::Sender<String>,
    pub(crate) runtime: Handle,
    pub(crate) worker: JoinHandle<()>,
}

impl fmt::Debug for VisualJobCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VisualJobCoordinator")
            .field("renderer_count", &self.core.renderers.len())
            .finish_non_exhaustive()
    }
}
