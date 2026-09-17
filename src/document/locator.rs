//! Locators back into a unit, and the rectangles and byte ranges they carry.

use super::*;

/// A stable location in a document. Text offsets are UTF-8 byte offsets within
/// `Block::plain_text`, and `end_byte` is exclusive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLocator {
    pub book_id: String,
    pub unit_id: String,
    pub block_id: Option<String>,
    pub text_range: Option<TextRange>,
    /// Native-source coordinate for citations that need more than a block or
    /// UTF-8 text span. Visual worksheet pages use this to retain their exact
    /// A1 range, while slide/page renderers retain the one-based native page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocator>,
    /// Optional visual sub-region in normalized 0..=1000 page coordinates.
    /// Existing locators without this field continue to identify the complete
    /// logical page or content unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<NormalizedRect>,
}

impl DocumentLocator {
    pub fn unit(book_id: impl Into<String>, unit_id: impl Into<String>) -> Self {
        Self {
            book_id: book_id.into(),
            unit_id: unit_id.into(),
            block_id: None,
            text_range: None,
            source: None,
            region: None,
        }
    }

    pub fn block(
        book_id: impl Into<String>,
        unit_id: impl Into<String>,
        block_id: impl Into<String>,
    ) -> Self {
        Self {
            book_id: book_id.into(),
            unit_id: unit_id.into(),
            block_id: Some(block_id.into()),
            text_range: None,
            source: None,
            region: None,
        }
    }

    pub fn text(
        book_id: impl Into<String>,
        unit_id: impl Into<String>,
        block_id: impl Into<String>,
        start_byte: u64,
        end_byte: u64,
    ) -> Self {
        Self {
            book_id: book_id.into(),
            unit_id: unit_id.into(),
            block_id: Some(block_id.into()),
            text_range: Some(TextRange::new(start_byte, end_byte)),
            source: None,
            region: None,
        }
    }

    pub fn with_source(mut self, source: SourceLocator) -> Self {
        self.source = Some(source);
        self
    }

    pub fn with_region(mut self, region: NormalizedRect) -> Self {
        self.region = Some(region);
        self
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("locator", &mut validator);
        validator.finish()
    }

    pub(crate) fn validate_into(&self, path: &str, validator: &mut Validator) {
        validate_id(validator, &format!("{path}.book_id"), &self.book_id);
        validate_id(validator, &format!("{path}.unit_id"), &self.unit_id);
        if let Some(block_id) = self.block_id.as_deref() {
            validate_id(validator, &format!("{path}.block_id"), block_id);
        }
        if let Some(range) = self.text_range {
            if self.block_id.is_none() {
                validator.issue(
                    format!("{path}.text_range"),
                    "a text range requires a block ID",
                );
            }
            if range.start_byte > range.end_byte {
                validator.issue(
                    format!("{path}.text_range"),
                    "start_byte must not exceed end_byte",
                );
            }
        }
        if let Some(source) = &self.source {
            source.validate_into(&format!("{path}.source"), validator);
        }
        if let Some(region) = self.region {
            region.validate_into(&format!("{path}.region"), validator);
        }
    }
}

/// A strict, positive-area rectangle in normalized page coordinates. The
/// right and bottom edges are exclusive for containment calculations, while
/// still using the inclusive coordinate domain 0..=1000.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedRect {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl NormalizedRect {
    pub const fn new(left: u16, top: u16, right: u16, bottom: u16) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("region", &mut validator);
        validator.finish()
    }

    pub(crate) fn validate_into(&self, path: &str, validator: &mut Validator) {
        if self.left > NORMALIZED_COORDINATE_MAX
            || self.top > NORMALIZED_COORDINATE_MAX
            || self.right > NORMALIZED_COORDINATE_MAX
            || self.bottom > NORMALIZED_COORDINATE_MAX
        {
            validator.issue(
                path,
                format!(
                    "normalized rectangle coordinates must be within 0..={NORMALIZED_COORDINATE_MAX}"
                ),
            );
        }
        if self.left >= self.right || self.top >= self.bottom {
            validator.issue(path, "normalized rectangle must have positive area");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub start_byte: u64,
    pub end_byte: u64,
}

impl TextRange {
    pub const fn new(start_byte: u64, end_byte: u64) -> Self {
        Self {
            start_byte,
            end_byte,
        }
    }
}
