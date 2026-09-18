//! Probe results, importer capabilities and the imported book shape.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProbeConfidence {
    NoMatch = 0,
    Extension = 20,
    Container = 60,
    Magic = 100,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    pub format: BookFormat,
    pub confidence: ProbeConfidence,
    pub detail: Option<String>,
}

impl ProbeResult {
    pub fn no_match(format: BookFormat) -> Self {
        Self {
            format,
            confidence: ProbeConfidence::NoMatch,
            detail: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImporterCapabilities {
    pub importer: &'static str,
    pub formats: &'static [BookFormat],
    pub parser_version: &'static str,
}

#[derive(Clone, Debug)]
pub struct ImportedAsset {
    pub metadata: AssetRef,
    pub bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct ImportedBook {
    pub document: BookDocument,
    pub assets: Vec<ImportedAsset>,
}

impl ImportedBook {
    pub fn validate(&self, limits: &ImportLimits) -> Result<()> {
        if self.assets.len() > limits.max_assets || self.document.assets.len() > limits.max_assets {
            bail!(
                "document exceeds the {} asset count safety limit",
                limits.max_assets
            );
        }
        self.document.validate()?;
        if self.document.units.len() > limits.max_units {
            bail!("document has too many content units");
        }
        let mut total = 0_usize;
        for unit in &self.document.units {
            if unit.source.len() > limits.max_unit_text_bytes {
                bail!("content unit {} exceeds the text safety limit", unit.id);
            }
            total = total
                .checked_add(unit.source.len())
                .context("total imported text size overflowed")?;
            if total > limits.max_total_text_bytes {
                bail!("document exceeds the total text safety limit");
            }
        }
        if self.assets.len() != self.document.assets.len() {
            bail!("imported asset payloads do not match document asset metadata");
        }
        let mut asset_budget = AssetBudget::new(limits);
        for asset in &self.assets {
            asset_budget.add_usize(asset.bytes.len(), &format!("asset {}", asset.metadata.id))?;
            if asset.bytes.len() as u64 != asset.metadata.byte_len {
                bail!(
                    "asset {} byte length does not match metadata",
                    asset.metadata.id
                );
            }
            let digest = blake3::hash(asset.bytes.as_slice()).to_hex().to_string();
            if digest != asset.metadata.content_hash {
                bail!(
                    "asset {} content hash does not match metadata",
                    asset.metadata.id
                );
            }
        }
        Ok(())
    }

    pub fn original_asset(&self) -> Option<&ImportedAsset> {
        self.assets
            .iter()
            .find(|asset| asset.metadata.roles.contains(&AssetRole::OriginalSource))
    }
}

pub trait DocumentImporter: Send + Sync {
    fn capabilities(&self) -> ImporterCapabilities;
    fn probe(&self, source: &ImportSource) -> ProbeResult;
    fn import(&self, source: &ImportSource, limits: &ImportLimits) -> Result<ImportedBook>;
}
