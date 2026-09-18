//! The Office units carried into a render request.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct OfficeUnit {
    pub(crate) id: String,
    pub(crate) revision: Revision,
    pub(crate) source_locator: Option<SourceLocator>,
}

pub(crate) fn office_units(
    request: &RenderUnitsRequest,
) -> Result<(Vec<OfficeUnit>, HashSet<String>)> {
    let requested = request.unit_ids.iter().cloned().collect::<HashSet<_>>();
    ensure!(
        requested.len() == request.unit_ids.len(),
        "Office 增强视觉任务不能重复选择内容单元"
    );
    let all = request
        .document
        .units
        .iter()
        .map(|unit| OfficeUnit {
            id: unit.id.clone(),
            revision: unit.revision,
            source_locator: unit.source_locator.clone(),
        })
        .collect::<Vec<_>>();
    ensure!(!all.is_empty(), "Office 增强视觉任务没有内容单元");
    if !requested.is_empty() {
        ensure!(
            requested
                .iter()
                .all(|unit_id| all.iter().any(|unit| unit.id == *unit_id)),
            "Office 增强视觉任务包含不存在的内容单元"
        );
    }
    Ok((all, requested))
}
