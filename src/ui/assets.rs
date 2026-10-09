//! The application asset bundle.
//!
//! GPUI Component ships the vector art for every `IconName`, including all the
//! icons the sidebar already renders. A few chat action icons are not part of
//! that set, so they live in this repository's `assets/icons/` and are served
//! from here first; every other path is delegated to the vendored bundle.
//!
//! Keeping the icons in one asset source (instead of drawing them from text or
//! shipping separate bundles) means every window and the GUI tests resolve the
//! exact same files.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// Vector art this repository maintains itself, keyed by the bundle path an
/// [`gpui_kit::Svg`] resolves through.
const LOCAL_ICONS: &[(&str, &[u8])] = &[
    (
        "icons/pencil.svg",
        include_bytes!("../../assets/icons/pencil.svg"),
    ),
    (
        "icons/refresh-cw.svg",
        include_bytes!("../../assets/icons/refresh-cw.svg"),
    ),
];

pub struct AppAssets;

impl AppAssets {
    fn local(path: &str) -> Option<Cow<'static, [u8]>> {
        LOCAL_ICONS
            .iter()
            .find(|(local, _)| *local == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes))
    }
}

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = Self::local(path) {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut listed = gpui_kit::assets::Assets.list(path)?;
        listed.extend(
            LOCAL_ICONS
                .iter()
                .filter(|(local, _)| local.starts_with(path))
                .map(|(local, _)| SharedString::from(*local)),
        );
        Ok(listed)
    }
}
