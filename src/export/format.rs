//! The export formats a book can be written as.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// The immutable source object. This is the only export that promises byte
    /// identity with an imported file.
    Original,
    /// A normalized EPUB 3 generated from the canonical block model.
    Epub,
    /// A self-contained PDF generated without an external Office/PDF program.
    Pdf,
}

impl ExportFormat {
    pub const fn extension(self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Epub => Some("epub"),
            Self::Pdf => Some("pdf"),
        }
    }

    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Original => "application/octet-stream",
            Self::Epub => "application/epub+zip",
            Self::Pdf => "application/pdf",
        }
    }
}
