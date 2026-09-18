//! Reference hints handed to the AI sidebar.

use super::*;

pub(crate) fn editor_reference_hints(
    book_id: &str,
    chapters: &[EditorChapter],
    unit_states: &[EditorUnitState],
    selected: usize,
    live_selected_title: Option<&str>,
    selected_text: Option<&str>,
    revision: u64,
) -> Vec<AiReferenceHint> {
    let mut indices = (0..chapters.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| (*index != selected, *index));
    indices
        .into_iter()
        .filter_map(|index| {
            let chapter = chapters.get(index)?;
            let unit_id = unit_states
                .get(index)
                .map(|unit| unit.id.clone())
                .unwrap_or_else(|| chapter.href.clone());
            let title = if index == selected {
                live_selected_title
                    .filter(|title| !title.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| chapter.title.clone())
            } else {
                chapter.title.clone()
            };
            let label = if index == selected {
                if selected_text.is_some() {
                    format!("当前章节高亮 · {title}")
                } else {
                    format!("当前章节 · {title}")
                }
            } else {
                title
            };
            let mut reference = AiReferenceHint::chapter(book_id, unit_id, index, label);
            if index == selected {
                reference.frozen_text = selected_text.map(str::to_string);
            }
            reference.revision = Some(revision);
            Some(reference)
        })
        .collect()
}

pub(crate) fn freeze_editor_reference_hints(
    book_id: &str,
    chapters: &[EditorChapter],
    unit_states: &[EditorUnitState],
    selected: usize,
    selected_text: Option<&str>,
    revision: u64,
    references: &mut [AiReferenceHint],
) {
    for reference in references {
        if reference.book_id != book_id {
            continue;
        }
        let index = unit_states
            .iter()
            .position(|unit| unit.id == reference.unit_id)
            .or(reference.unit_index)
            .filter(|index| *index < chapters.len());
        let Some(chapter) = index.and_then(|index| chapters.get(index)) else {
            continue;
        };
        reference.frozen_text = if index == Some(selected) {
            selected_text
                .map(str::to_string)
                .or_else(|| Some(searchable_text_from_xhtml(&chapter.html)))
        } else {
            Some(searchable_text_from_xhtml(&chapter.html))
        };
        if index == Some(selected) && selected_text.is_some() {
            let title = chapter.title.trim();
            reference.label = if title.is_empty() {
                "当前章节高亮".to_string()
            } else {
                format!("当前章节高亮 · {title}")
            };
        }
        reference.revision = Some(revision);
    }
}
