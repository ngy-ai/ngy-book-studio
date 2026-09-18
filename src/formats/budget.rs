//! The per-import asset budget.

use super::*;

/// Tracks the immutable payloads retained by one import. Importers use this
/// before cloning or accumulating extracted bytes; [`ImportedBook::validate`]
/// repeats the same checks over the final graph so custom importers cannot
/// bypass the limits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AssetBudget {
    pub(crate) max_assets: usize,
    pub(crate) max_asset_bytes: u64,
    pub(crate) max_total_asset_bytes: u64,
    pub(crate) assets: usize,
    pub(crate) total_bytes: u64,
}

impl AssetBudget {
    pub(crate) fn new(limits: &ImportLimits) -> Self {
        Self {
            max_assets: limits.max_assets,
            max_asset_bytes: limits.max_asset_bytes,
            max_total_asset_bytes: limits.max_total_asset_bytes,
            assets: 0,
            total_bytes: 0,
        }
    }

    pub(crate) fn with_original(
        limits: &ImportLimits,
        byte_len: usize,
        label: &str,
    ) -> Result<Self> {
        let mut budget = Self::new(limits);
        budget.add_usize(byte_len, label)?;
        Ok(budget)
    }

    pub(crate) fn with_original_u64(
        limits: &ImportLimits,
        byte_len: u64,
        label: &str,
    ) -> Result<Self> {
        let mut budget = Self::new(limits);
        budget.add(byte_len, label)?;
        Ok(budget)
    }

    pub(crate) fn check_single_usize(&self, byte_len: usize, label: &str) -> Result<u64> {
        let byte_len = u64::try_from(byte_len).context("asset byte length does not fit u64")?;
        self.check_single(byte_len, label)?;
        Ok(byte_len)
    }

    pub(crate) fn check_single(&self, byte_len: u64, label: &str) -> Result<()> {
        if byte_len > self.max_asset_bytes {
            bail!(
                "{label} exceeds the {} byte single-asset safety limit",
                self.max_asset_bytes
            );
        }
        Ok(())
    }

    pub(crate) fn add_usize(&mut self, byte_len: usize, label: &str) -> Result<()> {
        let byte_len = self.check_single_usize(byte_len, label)?;
        self.add_checked(byte_len, label)
    }

    pub(crate) fn add(&mut self, byte_len: u64, label: &str) -> Result<()> {
        self.check_single(byte_len, label)?;
        self.add_checked(byte_len, label)
    }

    pub(super) fn add_checked(&mut self, byte_len: u64, label: &str) -> Result<()> {
        let assets = self
            .assets
            .checked_add(1)
            .context("asset count overflowed")?;
        if assets > self.max_assets {
            bail!(
                "{label} exceeds the {} asset count safety limit",
                self.max_assets
            );
        }
        let total_bytes = self
            .total_bytes
            .checked_add(byte_len)
            .context("total imported asset size overflowed")?;
        if total_bytes > self.max_total_asset_bytes {
            bail!(
                "{label} exceeds the {} byte aggregate asset safety limit",
                self.max_total_asset_bytes
            );
        }
        self.assets = assets;
        self.total_bytes = total_bytes;
        Ok(())
    }
}
