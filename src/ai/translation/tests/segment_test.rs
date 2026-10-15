//! Tests for stored-translation shapes the walk must not accept as a译文.

use super::*;

fn segment(source: &str, translated: &str) -> TranslationSegment {
    TranslationSegment {
        source: source.to_string(),
        translated: translated.to_string(),
    }
}

fn stored(segments: Vec<TranslationSegment>) -> StoredTranslation {
    StoredTranslation {
        execution_identity: "translation-v2:chat-test".to_string(),
        segments,
    }
}

/// 模型把整块原文原样抄回来时，落盘的行看起来和一份真译文一模一样 —— 只有「拼起来的
/// 译文与原文逐字相同」能把它认出来。逐片段相同与只差空白都算（空白在渲染时本就被
/// 折叠，只差空白的「译文」与原文在读者眼里没有区别）。
#[test]
pub(crate) fn a_translation_that_echoes_the_source_is_recognized_as_a_passthrough() {
    assert!(
        stored(vec![
            segment("Type: Build", "Type: Build"),
            segment("Language: Python", "Language: Python"),
        ])
        .is_passthrough(),
        "逐片段原样返回是最直白的一种"
    );
    assert!(
        stored(vec![segment("Type: Build\n", "Type:  Build")]).is_passthrough(),
        "只差空白的「译文」与原文在读者眼里没有区别"
    );
    assert!(
        stored(vec![
            segment("Read important code()\n", "Read important code()\n"),
            segment("text", "text "),
        ])
        .is_passthrough(),
        "拼接之后只差空白同样算抄回来"
    );
}

/// 判定的反面必须保守：保留人名、URL、代码片段等局部原样是正常翻译的一部分，一份真
/// 译文绝不能因为「有几个片段没变」被当成没翻过 —— 那会让整本书反复重发。
#[test]
pub(crate) fn a_real_translation_is_never_mistaken_for_a_passthrough() {
    assert!(
        !stored(vec![segment(
            "GitHub is at github.com",
            "GitHub 见 github.com"
        )])
        .is_passthrough(),
        "换掉了词就是真译文，哪怕 URL 原样保留"
    );
    assert!(
        !stored(vec![
            segment("Read ", "阅读 "),
            segment("code() text", "code() text"),
        ])
        .is_passthrough(),
        "只有一个片段没变也是一份真译文"
    );
    assert!(
        !stored(vec![segment("定价是 20 元", "定价是 20 元。")]).is_passthrough(),
        "多一个标点就算改过原文"
    );
}
