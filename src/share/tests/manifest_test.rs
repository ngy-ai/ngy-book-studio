use super::super::*;

pub(crate) fn manifest() -> ShareManifest {
    ShareManifest {
        format: SHARE_FORMAT.to_string(),
        version: SHARE_VERSION,
        created_at: 7,
        book: ShareBookInfo {
            id: "book-1".into(),
            title: "共享之书".into(),
            author: "作者".into(),
            language: Some("en".into()),
            format: "epub".into(),
            units: 1,
        },
        notes: ShareSectionSummary { count: 0 },
        chats: ShareSectionSummary { count: 0 },
        translations: ShareTranslationSummary {
            count: 0,
            target_language: None,
        },
    }
}

#[test]
fn a_manifest_of_another_format_or_version_is_refused() {
    let mut other_format = manifest();
    other_format.format = "something-else".into();
    assert!(other_format.validate().is_err());

    let mut newer = manifest();
    newer.version = SHARE_VERSION + 1;
    let error = newer.validate().unwrap_err().to_string();
    assert!(
        error.contains("版本"),
        "a reader must name the version it cannot read: {error}"
    );

    assert!(manifest().validate().is_ok());
}

#[test]
fn a_manifest_without_a_title_or_with_too_many_units_is_refused() {
    let mut untitled = manifest();
    untitled.book.title = "   ".into();
    assert!(untitled.validate().is_err());

    let mut oversized = manifest();
    oversized.book.units = MAX_SHARE_UNITS + 1;
    assert!(oversized.validate().is_err());
}
