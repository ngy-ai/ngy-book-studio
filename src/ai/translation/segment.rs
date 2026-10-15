//! One stored translation segment.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationSegment {
    pub source: String,
    pub translated: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredTranslation {
    pub execution_identity: String,
    pub segments: Vec<TranslationSegment>,
}

impl StoredTranslation {
    /// 模型把这一块原样抄了回来：拼接后的译文与原文一致（只差空白）。
    ///
    /// 现场 2026-10-15：`Type: Build Language: Python Prerequisites: …` 这类元信息段落
    /// 被模型原文返回，系统当成有效译文落盘 —— 阅读端显示的仍是英文，而除了用户点
    /// 「重新翻译」以外没有任何入口会再碰它（游标已经走过去，缓存又把它算作「已翻」）。
    /// 判定因此保守到「逐字符相同（忽略空白）」：真正的译文不会与原文一字不差，而保留
    /// 人名、URL、代码片段等局部原样是正常翻译的一部分，绝不能被误判；空白在渲染时本就
    /// 被折叠，只差空白的「译文」与原文在读者眼里没有区别。
    pub(crate) fn is_passthrough(&self) -> bool {
        let same = |segment: &TranslationSegment| segment.translated == segment.source;
        if self.segments.iter().all(same) {
            return true;
        }
        let source = self
            .segments
            .iter()
            .map(|segment| segment.source.as_str())
            .collect::<String>();
        let translated = self
            .segments
            .iter()
            .map(|segment| segment.translated.as_str())
            .collect::<String>();
        let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        normalize(&source) == normalize(&translated)
    }
}
