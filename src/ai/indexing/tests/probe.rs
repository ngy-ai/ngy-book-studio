//! A window probe that records notifications.

use super::*;

/// 现场行为（2026-09-13，qwen3.5:0.8b）：模型会翻译，但把整段译文合并进一条
/// id=0 的片段；只有单片段请求才会得到合规答案。`fail_fragments` 让它对单片段
/// 单个请求的可观察窗口：并发用例要在请求真的打开时读一次持久游标。
pub(crate) struct WindowProbe {
    pub(crate) db_path: PathBuf,
    pub(crate) job_id: String,
    /// 每个请求开始时读到的 `(next_ordinal, inflight)`。
    pub(crate) windows: Mutex<Vec<(usize, Vec<usize>)>>,
}

impl WindowProbe {
    pub(crate) fn record(&self) {
        let Ok(conn) = db::open_conn(&self.db_path) else {
            return;
        };
        let Ok(Some(job)) = db::index_jobs::get(&conn, &self.job_id) else {
            return;
        };
        let Ok(cursor) = serde_json::from_str::<serde_json::Value>(&job.cursor_json) else {
            return;
        };
        let next_ordinal = cursor["next_ordinal"].as_u64().unwrap_or(0) as usize;
        let inflight = cursor["inflight"]
            .as_array()
            .map(|ordinals| {
                ordinals
                    .iter()
                    .filter_map(|ordinal| ordinal.as_u64())
                    .map(|ordinal| ordinal as usize)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.windows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((next_ordinal, inflight));
    }
}
