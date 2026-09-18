//! The vision output shape and its regions.

use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VisionOutput {
    pub(crate) regions: Vec<VisionRegion>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VisionRegion {
    pub(crate) ocr: String,
    pub(crate) description: String,
    // The custom deserializer keeps JSON `null` distinct from a missing field:
    // every model item must explicitly declare whether it is a full-page
    // fallback or a bounded region.
    #[serde(deserialize_with = "deserialize_required_vision_region")]
    pub(crate) region: Option<NormalizedRect>,
}

pub(crate) fn deserialize_required_vision_region<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<NormalizedRect>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<NormalizedRect>::deserialize(deserializer)
}

impl VisionRegion {
    pub(crate) const fn bounds(&self) -> Option<NormalizedRect> {
        self.region
    }
}
