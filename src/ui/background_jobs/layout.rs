//! Paging sizes and the list/detail pane widths.

use super::*;

pub(crate) const JOBS_PER_PAGE: usize = 12;
/// One page of the translation block inspector. Blocks are rendered without
/// virtual scrolling, so the page bounds what one frame has to build.
pub(crate) const BLOCKS_PER_PAGE: usize = 50;
/// Width the task list column starts at; the divider between the list and the
/// detail panel drags it between the hard bounds below.
pub(crate) const JOBS_LIST_DEFAULT_WIDTH: f32 = 380.;
pub(crate) const JOBS_LIST_MIN_WIDTH: f32 = 260.;
pub(crate) const JOBS_LIST_MAX_WIDTH: f32 = 640.;
/// The detail panel never renders narrower than this, so dragging the divider
/// can never squeeze the main subject out of the window.
pub(crate) const JOBS_DETAIL_MIN_WIDTH: f32 = 360.;
pub(crate) const JOBS_RESIZE_HANDLE_WIDTH: f32 = 6.;
pub(crate) const JOB_KINDS: &[(&str, &str)] = &[
    ("", "全部任务"),
    ("translation", "图书翻译"),
    ("embedding", "向量索引"),
    ("vision", "视觉理解"),
    ("visual_render", "页面渲染"),
    ("other", "其他任务"),
];
pub(crate) const STATUS_FILTERS: &[(Option<BackgroundJobStatus>, &str)] = &[
    (None, "全部状态"),
    (Some(BackgroundJobStatus::Running), "运行中"),
    (Some(BackgroundJobStatus::Queued), "排队中"),
    (Some(BackgroundJobStatus::Paused), "已暂停"),
    (Some(BackgroundJobStatus::Failed), "失败"),
    (Some(BackgroundJobStatus::Succeeded), "已完成"),
    (Some(BackgroundJobStatus::Cancelled), "已取消"),
];
