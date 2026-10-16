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

pub const FORMAT_INSTRUCTIONS: &str = "用户输入是 JSON：source 是完整文本块上下文，\
     segments 是允许翻译的文本片段。\
     带有 book 与 chapter 时它们是文本块所属图书的书名与章节标题，\
     只用来理解语境和术语，不要翻译、不要改写，也不要写进译文。\
     结合整个文本块翻译每个片段，只翻译 segments 给出的片段，\
     不要把一个片段的内容移到另一个片段，也不要翻译或补写 source 中不属于 segments 的内容。\
     只返回 JSON 对象 {\"translations\":[{\"id\":0,\"text\":\"译文\"}]}：\
     把 \"译文\" 替换成这一段的译文，不要增加其它字段、解释或 Markdown 围栏。\
     除声明不需要翻译的项外，每项只有 id 与 text 两个字段；\
     每个输入 id 必须且只能出现一次，id 使用整数。\
     数字与专有名词本身不改写，但要和其余文字一起给出译文。\
     只有当片段整段都没有需要翻译的文字时（纯符号、纯数字、公式、代码行、命令、\
     标识符与运算符组成的片段、函数调用、声明或注释），才把这一项写成 \
     {\"id\":0,\"text\":\"\",\"keep\":true}：text 必须恰好是空字符串，keep 必须为 true，\
     这一项保持原文。除此之外 text 一律必须是非空的译文，留空会被判为错误；\
     keep 只能出现在 text 为空字符串的项里，同一项既写 keep 又给出译文会被判为错误；\
     片段里只要还有需要翻译的词，就不要写 keep，直接给出译文。\
     text 的值必须是字符串：译文纯文本，不生成 HTML 或 Markdown 格式标记，\
     即使整段只是数字也要写成带引号的 \"188\" 这样的形式。\
     片段内部的换行在 JSON 里写成 \\n，应用会把它还原成换行；\
     片段首尾与相邻片段之间的空白由应用补回，不需要你处理。\
     JSON 的结构字符（花括号、方括号、双引号、冒号、逗号）必须使用半角 ASCII。\
     键名只能用不转义的半角双引号包裹，键名与值之间只能用半角冒号，各项之间只能用半角逗号；\
     值里的双引号恰好相反：必须转义写成 \\\"，反斜杠写成 \\\\，\
     例如 {\"id\":0,\"text\":\"他说 \\\"你好\\\"\"}。\
     全角标点（：，、；「」等）只能出现在 text 的值里面，绝不能写在键名、冒号或逗号的位置。\
     每一项都必须写成完整形式，不要省略键名、引号、冒号或逗号。";

impl TranslationSource {
    /// 用户消息体。`context` 里的书名与章节标题只作为语境字段附在同一个 JSON 里：
    /// 它们和正文走同一条不可信数据通道，模型不会把它们当成系统指令。
    pub fn request_input(&self, context: &TranslationContext<'_>) -> String {
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
        let (source, segments) = self.payload_fields();
        payload.insert("source".to_string(), source);
        payload.insert("segments".to_string(), segments);
        serde_json::Value::Object(payload).to_string()
    }

    /// 一个块自己的两个字段（`source` 与 `segments`）。
    ///
    /// 单块请求体与批请求里的每一项都从这里取，所以「同一个块单独发」与「放在批里发」
    /// 拿到的块负载逐字节相同：批只是外面多了一层信封。
    pub(crate) fn payload_fields(&self) -> (serde_json::Value, serde_json::Value) {
        let segments = self
            .segments
            .iter()
            .enumerate()
            .map(|(id, text)| serde_json::json!({"id": id, "text": text}))
            .collect::<Vec<_>>();
        (
            serde_json::Value::String(self.text.clone()),
            serde_json::Value::Array(segments),
        )
    }
}

/// 批信封的补充说明，接在 [`FORMAT_INSTRUCTIONS`] 后面。
///
/// 逐片段作答、`keep`、JSON 结构与转义、标点这些规则只写一份：批只是多了一层信封，
/// 所以这里只说明「这一次是多个块」与外层形状，两段拼起来就是批的全部协议文本 ——
/// 规则不会在单块与批两条路径之间漂移。
pub const FORMAT_INSTRUCTIONS_BATCH_ADDENDUM: &str = "这一次要翻译的不是一个文本块，而是用户输入里 blocks 数组中的多个文本块：\
     每一项都有自己的 id、source 与 segments。上面关于 source、segments 与 translations 的规则\
     逐块套用：每个块按它自己的 source 与 segments 作答，块与块之间不要合并、不要调换内容。\
     只返回 JSON 对象 {\"blocks\":[{\"id\":0,\"translations\":[{\"id\":0,\"text\":\"译文\"}]}]}：\
     外层的 id 是输入里那个块的 id，每个输入的块必须且只能出现一次；\
     内层的 translations 就是上面说的那一个数组，规则完全相同。";

/// 批请求的用户消息体：`blocks` 数组，每一项带自己的批内 `id`、`source` 与 `segments`。
///
/// 书名与章节标题取自第一项：调用方只把**同一个内容单元**（同一章）的块放进一批，
/// 这些语境字段对批里每一块都成立；混进另一章的块会让 `chapter` 撒谎。
///
/// `id` 是批内编号（`0..N`），与任务游标里的 ordinal 无关 —— 请求体因此与块在书里的位置
/// 无关，同一个块单独发与放进批里发，它自己那一份负载逐字节相同。
pub fn batch_request_input(items: &[TranslationBatchItem<'_>]) -> String {
    let mut payload = serde_json::Map::new();
    if let Some(first) = items.first() {
        for (field, value) in [
            ("book", first.context.book_title),
            ("chapter", first.context.chapter_title),
        ] {
            let label = context_label(value);
            if !label.is_empty() {
                payload.insert(field.to_string(), serde_json::Value::String(label));
            }
        }
    }
    let blocks = items
        .iter()
        .enumerate()
        .map(|(id, item)| {
            let mut block = serde_json::Map::new();
            block.insert("id".to_string(), serde_json::json!(id));
            let (source, segments) = item.source.payload_fields();
            block.insert("source".to_string(), source);
            block.insert("segments".to_string(), segments);
            serde_json::Value::Object(block)
        })
        .collect::<Vec<_>>();
    payload.insert("blocks".to_string(), serde_json::Value::Array(blocks));
    serde_json::Value::Object(payload).to_string()
}
