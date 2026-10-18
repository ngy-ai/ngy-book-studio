//! Window plumbing of the Endpoint settings window: the settings-view traits
//! and the root render.

use super::*;

impl SettingsWindowView for EndpointSettingsWindow {
    fn section() -> AiSettingsSection {
        AiSettingsSection::Endpoint
    }

    fn tracker(cx: &mut App) -> &mut SettingsWindowTracker<Self> {
        &mut cx.global_mut::<SettingsWindowTrackers>().endpoint
    }

    fn state(&self) -> &SettingsWindowState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut SettingsWindowState {
        &mut self.state
    }

    fn services(&self) -> &Arc<AppServices> {
        &self.services
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.busy() {
            return;
        }
        let settings = match self.entered_settings(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.state
                    .report(format!("Endpoint 设置无效：{error:#}"), true);
                cx.notify();
                return;
            }
        };
        let key_updates: BTreeMap<_, _> = self
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.id.clone(), endpoint.api_key_update(cx)))
            .collect();
        self.state.begin_save();
        commit_settings(
            Arc::clone(&self.services),
            settings,
            key_updates,
            ApiKeyUpdate::Keep,
            None,
            window,
            cx,
        );
    }
}

impl EndpointBackedWindow for EndpointSettingsWindow {
    fn endpoints(&self) -> &[EndpointDraft] {
        &self.endpoints
    }

    fn endpoints_mut(&mut self) -> &mut Vec<EndpointDraft> {
        &mut self.endpoints
    }
}

impl Render for EndpointSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_panel(cx);
        let footer_extra = self.render_footer_actions(cx);
        settings_frame(self, body, Some(footer_extra), cx)
    }
}
