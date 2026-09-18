//! Frozen selection snapshots and their citation identifiers.

use super::*;

/// A byte-exact copy of an unsaved editor selection at one editor revision.
/// It is deliberately separate from persistent search indexes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSnapshot {
    pub book_id: String,
    pub unit_id: String,
    /// Optional host-validated coordinate for page/block-specific references.
    /// The model can observe this value but cannot use it to widen the host
    /// scope or manufacture a citation that was not registered by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<DocumentLocator>,
    /// Editor-session revision from the WebView snapshot barrier. This is not
    /// the persisted [`crate::document::Revision`].
    pub revision: u64,
    pub content_hash: String,
    pub text: String,
    /// Citation identity and persisted model revisions issued only after the
    /// host has resolved this snapshot against the shared library. Older/raw
    /// snapshots remain deserializable but cannot be submitted to the runtime
    /// until the host authorizes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) host_citation: Option<SelectionCitationSource>,
}

/// Host-owned metadata that turns a byte-exact editor snapshot into a source
/// the model may cite. Book/unit IDs, locator and quote remain on the enclosing
/// [`SelectionSnapshot`] so there is only one copy of the frozen payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SelectionCitationSource {
    pub(crate) citation_id: String,
    pub(crate) document_revision: Revision,
    pub(crate) unit_revision: Revision,
    pub(crate) book_title: String,
    pub(crate) unit_title: String,
}

impl SelectionSnapshot {
    pub fn capture(
        book_id: impl Into<String>,
        unit_id: impl Into<String>,
        revision: u64,
        text: impl Into<String>,
    ) -> AgentResult<Self> {
        let book_id = book_id.into();
        let unit_id = unit_id.into();
        let text = text.into();
        validate_identifier("book_id", &book_id)?;
        validate_identifier("unit_id", &unit_id)?;
        if text.is_empty() {
            return Err(AgentError::InvalidArguments(
                "snapshot text must not be empty".to_string(),
            ));
        }
        Ok(Self {
            book_id,
            unit_id,
            locator: None,
            revision,
            content_hash: snapshot_hash(text.as_bytes()),
            text,
            host_citation: None,
        })
    }

    pub fn restore(
        book_id: impl Into<String>,
        unit_id: impl Into<String>,
        revision: u64,
        content_hash: impl Into<String>,
        text: impl Into<String>,
    ) -> AgentResult<Self> {
        let snapshot = Self {
            book_id: book_id.into(),
            unit_id: unit_id.into(),
            locator: None,
            revision,
            content_hash: content_hash.into(),
            text: text.into(),
            host_citation: None,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn with_locator(mut self, locator: DocumentLocator) -> AgentResult<Self> {
        self.locator = Some(locator);
        // A locator is part of the citation identity. Changing it invalidates
        // any authorization previously issued for this snapshot.
        self.host_citation = None;
        self.validate()?;
        Ok(self)
    }

    /// Resolves this frozen selection against host-owned document metadata and
    /// issues the only citation ID that the runtime will accept for it.
    pub(crate) fn with_host_citation(
        mut self,
        locator: DocumentLocator,
        document_revision: Revision,
        unit_revision: Revision,
        book_title: impl Into<String>,
        unit_title: impl Into<String>,
    ) -> AgentResult<Self> {
        self.locator = Some(locator);
        self.host_citation = None;
        self.validate()?;

        let book_title = book_title.into();
        let unit_title = unit_title.into();
        let citation_id = selection_citation_id(
            &self,
            document_revision,
            unit_revision,
            &book_title,
            &unit_title,
        )?;
        self.host_citation = Some(SelectionCitationSource {
            citation_id,
            document_revision,
            unit_revision,
            book_title,
            unit_title,
        });
        self.validate()?;
        Ok(self)
    }

    /// Returns the complete, validated source that can be registered before a
    /// model call. Raw editor snapshots deliberately have no such source.
    pub(crate) fn citation(&self) -> AgentResult<AgentCitation> {
        self.validate()?;
        let source = self.host_citation.as_ref().ok_or_else(|| {
            AgentError::InvalidArguments(
                "snapshot has not been authorized by the host as a citation".to_string(),
            )
        })?;
        Ok(AgentCitation {
            citation_id: source.citation_id.clone(),
            book_id: self.book_id.clone(),
            book_title: source.book_title.clone(),
            unit_id: self.unit_id.clone(),
            unit_title: source.unit_title.clone(),
            document_revision: source.document_revision,
            unit_revision: source.unit_revision,
            quote: self.text.clone(),
            locator: self
                .locator
                .clone()
                .expect("validated host citation locator"),
            source_kind: AgentCitationSourceKind::Book,
            url: None,
            source_title: None,
        })
    }

    pub fn validate(&self) -> AgentResult<()> {
        validate_identifier("book_id", &self.book_id)?;
        validate_identifier("unit_id", &self.unit_id)?;
        if let Some(locator) = self.locator.as_ref() {
            locator
                .validate()
                .map_err(|error| AgentError::InvalidArguments(error.to_string()))?;
            if locator.book_id != self.book_id || locator.unit_id != self.unit_id {
                return Err(AgentError::InvalidArguments(
                    "snapshot locator must match its book and content unit".to_string(),
                ));
            }
        }
        if self.text.is_empty() {
            return Err(AgentError::InvalidArguments(
                "snapshot text must not be empty".to_string(),
            ));
        }
        if self.content_hash != snapshot_hash(self.text.as_bytes()) {
            return Err(AgentError::InvalidArguments(
                "snapshot content hash does not match its frozen text".to_string(),
            ));
        }
        if let Some(source) = self.host_citation.as_ref() {
            let locator = self.locator.as_ref().ok_or_else(|| {
                AgentError::InvalidArguments(
                    "host-authorized snapshot must have a locator".to_string(),
                )
            })?;
            validate_identifier("snapshot citation_id", &source.citation_id)?;
            let expected = selection_citation_id(
                self,
                source.document_revision,
                source.unit_revision,
                &source.book_title,
                &source.unit_title,
            )?;
            if source.citation_id != expected {
                return Err(AgentError::InvalidArguments(
                    "snapshot citation ID does not match its host metadata".to_string(),
                ));
            }
            debug_assert_eq!(locator.book_id, self.book_id);
            debug_assert_eq!(locator.unit_id, self.unit_id);
        }
        Ok(())
    }
}

pub(crate) fn selection_citation_id(
    snapshot: &SelectionSnapshot,
    document_revision: Revision,
    unit_revision: Revision,
    book_title: &str,
    unit_title: &str,
) -> AgentResult<String> {
    let locator = snapshot.locator.as_ref().ok_or_else(|| {
        AgentError::InvalidArguments("host-authorized snapshot must have a locator".to_string())
    })?;
    let locator = serde_json::to_vec(locator)
        .map_err(|error| AgentError::InvalidArguments(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ngy-agent-selection-citation-v1\0");
    for bytes in [
        snapshot.book_id.as_bytes(),
        snapshot.unit_id.as_bytes(),
        &locator,
        &snapshot.revision.to_le_bytes(),
        snapshot.content_hash.as_bytes(),
        &document_revision.get().to_le_bytes(),
        &unit_revision.get().to_le_bytes(),
        book_title.as_bytes(),
        unit_title.as_bytes(),
    ] {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    Ok(format!("selection:{}", hasher.finalize().to_hex().as_str()))
}

pub(crate) fn snapshot_hash(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ngy-agent-selection-v1\0");
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn selection_marker_nonce() -> AgentResult<String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|error| {
        AgentError::Backend(format!("could not generate citation nonce: {error}"))
    })?;
    pub(super) const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    Ok(encoded)
}
