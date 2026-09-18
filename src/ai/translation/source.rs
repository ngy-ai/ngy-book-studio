//! Where a translated block comes from and the context it carries.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationSource {
    /// Normalized visible text, including code, used to match the source block.
    pub text: String,
    /// Non-blank text leaves in document order, with original boundary spacing.
    pub segments: Vec<String>,
}

/// 随每个文本块一起发给模型的图书上下文：书名与当前章节标题。
///
/// 两者都只帮助模型判断语境、人名与术语，**不产生译文**：它们既不参与片段编号，
/// 也不进入译文输出。它们和正文一样是不可信图书数据，因此只出现在用户消息的
/// JSON 里（那里的每一部分都已声明为不可信），不拼进系统提示。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranslationContext<'a> {
    pub book_title: &'a str,
    pub chapter_title: &'a str,
}

impl<'a> TranslationContext<'a> {
    pub fn new(book_title: &'a str, chapter_title: &'a str) -> Self {
        Self {
            book_title,
            chapter_title,
        }
    }
}

/// 书名与章节标题在请求体里的最大字符数。它们是语境信息而不是待翻译内容，
/// 截断只可能丢掉过长的标题，却保证上下文只占请求体里一块固定的少量字节 ——
/// 否则一个畸形（或恶意）的长标题会让整本书的每个文本块都超过请求上限。
pub(crate) const MAX_CONTEXT_LABEL_CHARS: usize = 200;

/// 上下文字段在请求体里的形态：折叠空白并截断。空标题会被整段省略，因此没有
/// 上下文的调用（例如逐片段回退的合成片段）产出的请求体与不带上下文时逐字节相同。
pub(crate) fn context_label(value: &str) -> String {
    let normalized = normalize_source_text(value);
    if normalized.chars().count() <= MAX_CONTEXT_LABEL_CHARS {
        return normalized;
    }
    normalized.chars().take(MAX_CONTEXT_LABEL_CHARS).collect()
}
