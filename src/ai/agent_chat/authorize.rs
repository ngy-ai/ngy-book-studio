//! Authorizing the frozen selection snapshots a question carries.

use super::*;

pub(crate) fn authorize_selection_snapshots(
    library: &LibraryStore,
    scope: &ChatScope,
    snapshots: Vec<SelectionSnapshot>,
) -> Result<Vec<SelectionSnapshot>> {
    snapshots
        .into_iter()
        .map(|snapshot| {
            snapshot.validate().map_err(anyhow::Error::new)?;
            ensure!(
                scope.contains(&snapshot.book_id),
                "frozen selection is outside the host-authorized scope"
            );
            let document = library
                .document(&snapshot.book_id)
                .context("frozen selection book no longer exists")?;
            let unit = document
                .find_unit(&snapshot.unit_id)
                .context("frozen selection content unit no longer exists")?;
            let locator = snapshot.locator.clone().unwrap_or_else(|| {
                let locator = DocumentLocator::unit(&document.id, &unit.id);
                match unit.source_locator.clone() {
                    Some(source) => locator.with_source(source),
                    None => locator,
                }
            });
            if let Some(source) = locator.source.as_ref() {
                ensure!(
                    !matches!(source, SourceLocator::OfficeRenderedPage { .. }),
                    "frozen selection cannot cite a preview-only Office rendered page"
                );
                ensure!(
                    unit.source_locator.as_ref() == Some(source),
                    "frozen selection source locator does not match the current content unit"
                );
            }
            document
                .validate_locator(&locator)
                .context("frozen selection locator is stale or invalid")?;
            snapshot
                .with_host_citation(
                    locator,
                    document.revision,
                    unit.revision,
                    document.title.clone(),
                    unit.title.clone(),
                )
                .map_err(anyhow::Error::new)
        })
        .collect()
}
