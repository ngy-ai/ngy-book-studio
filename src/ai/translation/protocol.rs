//! The execution-identity protocol tag and the response format prompt.

use super::*;

/// Protocol version of the persisted structured translation format. The
/// endpoint/model digest after the colon is deliberately excluded: rows written
/// by another engine may keep their text as long as the same protocol can read
/// them, while a newer protocol invalidates every stored row.
pub const EXECUTION_IDENTITY_PROTOCOL: &str = "translation-v2";

/// Protocol prefix of one execution identity, i.e. everything before the
/// endpoint/model digest. Unknown shapes are returned unchanged so two
/// unexpected identities only match when they are literally equal.
pub fn execution_identity_protocol(identity: &str) -> &str {
    identity.split(':').next().unwrap_or_default()
}

pub const FORMAT_INSTRUCTIONS: &str = "用户输入是 JSON，book 与 chapter 是文本块所属图书的书名与章节标题，\
     source 是完整文本块上下文，segments 是允许翻译的文本片段。\
     book 与 chapter 只用来理解语境和术语，不要翻译、不要改写，也不要写进译文。\
     结合整个文本块翻译每个片段，只翻译 segments 给出的片段，\
     不要把一个片段的内容移到另一个片段，也不要翻译或补写 source 中不属于 segments 的内容。\
     仅返回 JSON 对象 {\"translations\":[{\"id\":0,\"text\":\"译文\"}]}。\
     除声明不需要翻译的项外，每项只有 id 与 text 两个字段；\
     每个输入 id 必须且只能出现一次，id 使用整数；\
     不得增加其它字段、解释或 Markdown 围栏。\
     数字与专有名词本身不改写，但要和其余文字一起给出译文。\
     只有当片段整段都没有需要翻译的文字时（纯符号、纯数字、公式、代码行、命令、\
     标识符与运算符组成的片段、函数调用、声明或注释），才把这一项写成 \
     {\"id\":0,\"text\":\"\",\"keep\":true}：text 必须恰好是空字符串，keep 必须为 true，\
     这一项保持原文。除此之外 text 一律必须是非空的译文，留空会被判为错误。\
     keep 只能出现在 text 为空字符串的项里，同一项既写 keep 又给出译文会被判为错误；\
     片段里只要还有需要翻译的词，就不要写 keep，直接给出译文。\
     text 只能是译文纯文本，不生成 HTML 或 Markdown 格式标记。\
     片段内部的换行照原样保留；片段首尾与相邻片段之间的空白由应用补回，不需要你处理。\
     JSON 的结构字符（花括号、方括号、双引号、冒号、逗号）必须使用半角 ASCII：\
     键名只能用半角双引号包裹，键名与值之间只能用半角冒号，各项之间只能用半角逗号。\
     全角标点（：，、；「」等）只能出现在 text 的值里面，绝不能写在键名、冒号或逗号的位置。\
     每个片段都必须写成 {\"id\":0,\"text\":\"译文\"} 或 {\"id\":0,\"text\":\"\",\"keep\":true} 这样的完整形式，\
     不要省略键名、引号、冒号或逗号。";

impl TranslationSource {
    /// 用户消息体。`context` 里的书名与章节标题只作为语境字段附在同一个 JSON 里：
    /// 它们和正文走同一条不可信数据通道，模型不会把它们当成系统指令。
    pub fn request_input(&self, context: &TranslationContext<'_>) -> String {
        let segments = self
            .segments
            .iter()
            .enumerate()
            .map(|(id, text)| serde_json::json!({"id": id, "text": text}))
            .collect::<Vec<_>>();
        let mut payload = serde_json::Map::new();
        for (field, value) in [
            ("book", context.book_title),
            ("chapter", context.chapter_title),
        ] {
            let label = context_label(value);
            if !label.is_empty() {
                payload.insert(field.to_string(), serde_json::Value::String(label));
            }
        }
        payload.insert(
            "source".to_string(),
            serde_json::Value::String(self.text.clone()),
        );
        payload.insert("segments".to_string(), serde_json::Value::Array(segments));
        serde_json::Value::Object(payload).to_string()
    }
}
