//! Referenced assets and the roles they fulfil.

use super::*;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetRole {
    OriginalSource,
    Cover,
    ContentImage,
    Audio,
    Video,
    Poster,
    Attachment,
    Font,
    Stylesheet,
}

/// Storage-neutral metadata for an immutable asset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    pub id: String,
    pub roles: Vec<AssetRole>,
    pub media_type: String,
    pub original_file_name: Option<String>,
    pub byte_len: u64,
    pub content_hash: String,
}

impl AssetRef {
    pub fn new(
        id: impl Into<String>,
        roles: Vec<AssetRole>,
        media_type: impl Into<String>,
        original_file_name: Option<String>,
        byte_len: u64,
        content_hash: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            roles,
            media_type: media_type.into(),
            original_file_name,
            byte_len,
            content_hash: content_hash.into(),
        }
    }

    pub fn from_bytes(
        role: AssetRole,
        media_type: impl Into<String>,
        original_file_name: Option<String>,
        bytes: &[u8],
    ) -> Self {
        let content_hash = blake3::hash(bytes).to_hex().to_string();
        Self {
            id: format!("asset-{content_hash}"),
            roles: vec![role],
            media_type: media_type.into(),
            original_file_name,
            byte_len: bytes.len() as u64,
            content_hash,
        }
    }

    pub fn has_role(&self, role: AssetRole) -> bool {
        self.roles.contains(&role)
    }

    pub fn add_role(&mut self, role: AssetRole) {
        if !self.has_role(role) {
            self.roles.push(role);
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut validator = Validator::default();
        self.validate_into("asset", &mut validator);
        validator.finish()
    }

    pub(crate) fn validate_into(&self, path: &str, validator: &mut Validator) {
        validate_id(validator, &format!("{path}.id"), &self.id);
        if self.roles.is_empty() {
            validator.issue(format!("{path}.roles"), "at least one role is required");
        }
        let mut roles = HashSet::new();
        for (index, role) in self.roles.iter().copied().enumerate() {
            if !roles.insert(role) {
                validator.issue(format!("{path}.roles[{index}]"), "duplicate asset role");
            }
        }
        if self.media_type.trim().is_empty()
            || !self.media_type.contains('/')
            || self.media_type.chars().any(char::is_control)
        {
            validator.issue(format!("{path}.media_type"), "invalid media type");
        }
        validate_optional_file_name(
            validator,
            &format!("{path}.original_file_name"),
            self.original_file_name.as_deref(),
        );
        if self.content_hash.len() != 64
            || !self
                .content_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            validator.issue(
                format!("{path}.content_hash"),
                "content hash must be a 64-character hexadecimal BLAKE3 digest",
            );
        }
        for role in &self.roles {
            let compatible = match role {
                AssetRole::Cover | AssetRole::ContentImage | AssetRole::Poster => {
                    self.media_type.starts_with("image/")
                        || self.media_type == "application/svg+xml"
                }
                AssetRole::Audio => self.media_type.starts_with("audio/"),
                AssetRole::Video => self.media_type.starts_with("video/"),
                AssetRole::Stylesheet => self.media_type == "text/css",
                AssetRole::OriginalSource | AssetRole::Attachment | AssetRole::Font => true,
            };
            if !compatible {
                validator.issue(
                    format!("{path}.media_type"),
                    format!("media type is incompatible with the {role:?} role"),
                );
            }
        }
    }
}
