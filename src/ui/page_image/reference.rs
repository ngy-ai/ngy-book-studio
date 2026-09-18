//! Resolving a source to a page and building the reference hints.

use super::*;

pub(crate) fn page_index_for_source(
    pages: &[PageImage],
    kind: VisualPageSourceKind,
    current: usize,
    source: &AiSourceLink,
) -> Option<usize> {
    if source.stale {
        return None;
    }
    if let Some(locator) = source.validated_locator()
        && let Some(source_locator) = locator.source.as_ref()
    {
        match source_locator {
            SourceLocator::OfficeRenderedPage { .. } => return None,
            source_locator if is_exact_page_source(kind, source_locator) => {
                let mut matching = pages
                    .iter()
                    .enumerate()
                    .filter(|(_, page)| page.locator == *locator)
                    .map(|(index, _)| index);
                let page_index = matching.next()?;
                return matching.next().is_none().then_some(page_index);
            }
            _ => return None,
        }
    }
    if source.locator.is_some() && source.validated_locator().is_none() {
        return None;
    }
    pages
        .get(current)
        .filter(|page| page.locator.unit_id == source.unit_id)
        .map(|_| current)
        .or_else(|| {
            pages
                .iter()
                .position(|page| page.locator.unit_id == source.unit_id)
        })
}

/// Source coordinates that identify exactly one page in a page-image set.
pub(crate) fn is_exact_page_source(kind: VisualPageSourceKind, source: &SourceLocator) -> bool {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => matches!(
            source,
            SourceLocator::Slide { .. }
                | SourceLocator::OfficeSection { .. }
                | SourceLocator::Worksheet { .. }
        ),
        VisualPageSourceKind::Djvu => matches!(source, SourceLocator::DjvuPage { .. }),
    }
}

pub(crate) fn page_has_exact_unit_mapping(page: &PageImage) -> bool {
    !matches!(
        page.locator.source.as_ref(),
        Some(SourceLocator::OfficeRenderedPage { .. })
    )
}

/// AI references for a page set. Only pages with an exact content-unit mapping
/// become references: Office repagination previews deliberately stay out, while
/// every DjVu page maps 1:1 onto its page unit.
pub(crate) fn page_reference_hints(
    book_id: &str,
    pages: &[PageImage],
    current: usize,
) -> Vec<AiReferenceHint> {
    let mut indices = (0..pages.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| (*index != current, *index));
    indices
        .into_iter()
        .filter(|index| page_has_exact_unit_mapping(&pages[*index]))
        .map(|index| {
            let page = &pages[index];
            AiReferenceHint {
                book_id: book_id.to_string(),
                unit_id: page.locator.unit_id.clone(),
                unit_index: None,
                locator: Some(page.locator.clone()),
                label: if index == current {
                    format!("当前页面 · 第 {} 页", page.page_number)
                } else {
                    format!("第 {} 页", page.page_number)
                },
                frozen_text: None,
                displayed_text: None,
                revision: None,
            }
        })
        .collect::<Vec<_>>()
}

pub(crate) fn page_subtitle(page: &PageImage, kind: VisualPageSourceKind) -> String {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => {
            if page_has_exact_unit_mapping(page) {
                format!(
                    "Microsoft Office 只读导出的持久化增强页面 · {}",
                    page.file_name
                )
            } else {
                format!(
                    "Microsoft Office 只读导出的增强预览页 · {} · 与章节/工作表无精确对应，仅供预览，不用于搜索或 AI 引用",
                    page.file_name
                )
            }
        }
        VisualPageSourceKind::Djvu => {
            format!(
                "DjVu 页面 · {} · 第 {} 页",
                page.file_name, page.page_number
            )
        }
    }
}
