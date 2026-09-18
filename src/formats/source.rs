//! The source handed to an importer.

use super::*;

#[derive(Clone, Debug)]
pub struct ImportSource {
    pub file_name: Option<String>,
    pub bytes: Arc<Vec<u8>>,
}

impl ImportSource {
    pub fn from_path(path: &Path, limits: &ImportLimits) -> Result<Self> {
        let metadata = path
            .metadata()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if !metadata.is_file() {
            bail!("import source is not a regular file: {}", path.display());
        }
        if metadata.len() > limits.max_source_bytes {
            bail!(
                "import source is larger than the {} byte safety limit",
                limits.max_source_bytes
            );
        }
        AssetBudget::with_original_u64(limits, metadata.len(), "import source")?;
        let file =
            fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let initial_capacity = usize::try_from(metadata.len().min(8 * 1024 * 1024)).unwrap_or(0);
        let mut bytes = Vec::with_capacity(initial_capacity);
        file.take(limits.max_source_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if bytes.len() as u64 > limits.max_source_bytes {
            bail!(
                "import source is larger than the {} byte safety limit",
                limits.max_source_bytes
            );
        }
        AssetBudget::with_original(limits, bytes.len(), "import source")?;
        Ok(Self {
            file_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            bytes: Arc::new(bytes),
        })
    }

    pub fn extension(&self) -> Option<String> {
        self.file_name
            .as_deref()
            .and_then(|name| Path::new(name).extension())
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
    }

    pub fn stem(&self) -> String {
        self.file_name
            .as_deref()
            .and_then(|name| Path::new(name).file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or("Untitled")
            .to_string()
    }
}
