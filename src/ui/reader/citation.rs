//! Resolving a citation target to a reflowable page.

use super::*;

pub(crate) fn reflowable_citation_target(
    source: &AiSourceLink,
    document: &BookDocument,
    opened: &OpenedBook,
) -> std::result::Result<ReflowableCitationTarget, String> {
    let target = source.current_navigation_target(document)?;
    let spine = opened
        .spine
        .get(target.unit_index)
        .ok_or_else(|| "引用对应的阅读章节已不存在".to_string())?;
    let href = match target.source.as_ref() {
        Some(SourceLocator::Epub { href }) => {
            if opened.spine_index_for_url(&OpenedBook::url_for_href(href))
                != Some(target.unit_index)
            {
                return Err("引用的 EPUB 地址与当前内容单元不匹配".to_string());
            }
            href.clone()
        }
        _ => spine.href.clone(),
    };
    Ok(ReflowableCitationTarget {
        spine_index: target.unit_index,
        href,
        focus: target.focus,
    })
}
