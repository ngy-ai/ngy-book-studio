//! Optional Microsoft Office preview enhancement on a dedicated worker.
//!
//! The canonical structured preview never depends on Office. A user may opt a
//! book into this best-effort path; errors are returned to the caller so it can
//! fall back without changing the original file.

use std::{
    ffi::OsStr,
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};
use tokio::sync::oneshot;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfficeEnhancementKind {
    WordPdf,
    ExcelPdf,
    PowerPointImages,
}

impl OfficeEnhancementKind {
    fn command_name(self) -> &'static str {
        match self {
            Self::WordPdf => "word_pdf",
            Self::ExcelPdf => "excel_pdf",
            Self::PowerPointImages => "powerpoint_images",
        }
    }

    fn accepts(self, extension: &str) -> bool {
        match self {
            Self::WordPdf => matches!(extension, "doc" | "docx"),
            Self::ExcelPdf => extension == "xlsx",
            Self::PowerPointImages => extension == "pptx",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OfficeCancellation(Arc<AtomicBool>);

impl OfficeCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
pub struct OfficeEnhanceRequest {
    pub kind: OfficeEnhancementKind,
    pub source: PathBuf,
    /// Word/Excel: a `.pdf` file. PowerPoint: an existing, empty directory.
    pub target: PathBuf,
    pub timeout: Duration,
    pub cancellation: OfficeCancellation,
}

impl OfficeEnhanceRequest {
    pub fn new(kind: OfficeEnhancementKind, source: PathBuf, target: PathBuf) -> Self {
        Self {
            kind,
            source,
            target,
            timeout: DEFAULT_TIMEOUT,
            cancellation: OfficeCancellation::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfficeEnhanceOutput {
    Pdf(PathBuf),
    Images(Vec<PathBuf>),
}

pub type OfficeFuture<'a> = Pin<Box<dyn Future<Output = Result<OfficeEnhanceOutput>> + Send + 'a>>;

pub trait OfficeEnhancer: Send + Sync {
    fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a>;
}

struct WorkItem {
    request: OfficeEnhanceRequest,
    reply: oneshot::Sender<Result<OfficeEnhanceOutput>>,
}

/// Serializes all Office automation onto one dedicated STA PowerShell process
/// at a time. GPUI and the Tokio I/O workers are never blocked by COM calls.
#[derive(Clone)]
pub struct OfficeComWorker {
    sender: mpsc::Sender<WorkItem>,
}

impl std::fmt::Debug for OfficeComWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OfficeComWorker")
            .finish_non_exhaustive()
    }
}

impl OfficeComWorker {
    pub fn start() -> Result<Self> {
        let (sender, receiver) = mpsc::channel::<WorkItem>();
        thread::Builder::new()
            .name("ngy-office-sta".to_string())
            .spawn(move || {
                while let Ok(work) = receiver.recv() {
                    let result = execute_request(&work.request);
                    let _ = work.reply.send(result);
                }
            })
            .context("failed to start the Office STA worker")?;
        Ok(Self { sender })
    }
}

impl OfficeEnhancer for OfficeComWorker {
    fn enhance<'a>(&'a self, request: OfficeEnhanceRequest) -> OfficeFuture<'a> {
        Box::pin(async move {
            let (reply, receiver) = oneshot::channel();
            self.sender
                .send(WorkItem { request, reply })
                .context("Office STA worker has stopped")?;
            receiver
                .await
                .context("Office STA worker dropped a request")?
        })
    }
}

fn execute_request(request: &OfficeEnhanceRequest) -> Result<OfficeEnhanceOutput> {
    validate_request(request)?;
    ensure!(
        !request.cancellation.is_cancelled(),
        "Office preview was cancelled"
    );

    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-Command",
            OFFICE_SCRIPT,
        ])
        .env("NGY_OFFICE_SOURCE", &request.source)
        .env("NGY_OFFICE_TARGET", &request.target)
        .env("NGY_OFFICE_KIND", request.kind.command_name())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_child_window(&mut command);
    let mut child = command
        .spawn()
        .context("failed to start Windows PowerShell for the optional Office preview")?;
    let started = Instant::now();
    loop {
        if request.cancellation.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Office preview was cancelled");
        }
        if started.elapsed() >= request.timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Office preview timed out after {:?}", request.timeout);
        }
        if let Some(status) = child.try_wait().context("failed to poll Office preview")? {
            ensure!(
                status.success(),
                "Microsoft Office preview was unavailable or failed; use the structured preview"
            );
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }

    match request.kind {
        OfficeEnhancementKind::WordPdf | OfficeEnhancementKind::ExcelPdf => {
            ensure!(
                request.target.is_file(),
                "Office did not produce the expected PDF"
            );
            ensure!(
                request.target.metadata()?.len() > 0,
                "Office produced an empty PDF"
            );
            Ok(OfficeEnhanceOutput::Pdf(request.target.clone()))
        }
        OfficeEnhancementKind::PowerPointImages => {
            let mut images = fs::read_dir(&request.target)?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.extension()
                        .and_then(OsStr::to_str)
                        .is_some_and(|extension| {
                            extension.eq_ignore_ascii_case("png")
                                || extension.eq_ignore_ascii_case("jpg")
                                || extension.eq_ignore_ascii_case("jpeg")
                        })
                })
                .collect::<Vec<_>>();
            images.sort();
            ensure!(!images.is_empty(), "Office did not produce slide images");
            Ok(OfficeEnhanceOutput::Images(images))
        }
    }
}

fn validate_request(request: &OfficeEnhanceRequest) -> Result<()> {
    ensure!(
        request.timeout > Duration::ZERO,
        "Office timeout must be positive"
    );
    ensure!(
        request.source.is_absolute(),
        "Office source must be absolute"
    );
    ensure!(request.source.is_file(), "Office source does not exist");
    let extension = request
        .source
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    ensure!(
        request.kind.accepts(&extension),
        "Office enhancement kind does not match the source format"
    );
    ensure!(
        request.target.is_absolute(),
        "Office target must be absolute"
    );
    ensure!(
        request.target != request.source,
        "Office target cannot be the original file"
    );
    match request.kind {
        OfficeEnhancementKind::WordPdf | OfficeEnhancementKind::ExcelPdf => {
            ensure!(
                request
                    .target
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf")),
                "Word/Excel enhancement target must be a PDF file"
            );
            ensure!(
                request.target.parent().is_some_and(Path::is_dir),
                "Office output directory does not exist"
            );
        }
        OfficeEnhancementKind::PowerPointImages => {
            ensure!(
                request.target.is_dir(),
                "PowerPoint target must be a directory"
            );
            ensure!(
                fs::read_dir(&request.target)?.next().is_none(),
                "PowerPoint target directory must be empty"
            );
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn hide_child_window(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn hide_child_window(_: &mut Command) {}

/// Fixed script: paths are supplied through environment variables rather than
/// interpolated into executable text. Macro execution is force-disabled (3),
/// documents are opened read-only, link updates are disabled, and originals
/// are only ever exported to a separate target.
const OFFICE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$source = [Environment]::GetEnvironmentVariable('NGY_OFFICE_SOURCE')
$target = [Environment]::GetEnvironmentVariable('NGY_OFFICE_TARGET')
$kind = [Environment]::GetEnvironmentVariable('NGY_OFFICE_KIND')
$app = $null
$document = $null
try {
  switch ($kind) {
    'word_pdf' {
      $app = New-Object -ComObject Word.Application
      $app.Visible = $false
      $app.DisplayAlerts = 0
      $app.AutomationSecurity = 3
      $document = $app.Documents.Open($source, $false, $true, $false, '', '', $false, '', '', 0, $false, $false, $false, $true, $false, $false)
      $document.ExportAsFixedFormat($target, 17)
    }
    'excel_pdf' {
      $app = New-Object -ComObject Excel.Application
      $app.Visible = $false
      $app.DisplayAlerts = $false
      $app.AskToUpdateLinks = $false
      $app.AutomationSecurity = 3
      $document = $app.Workbooks.Open($source, 0, $true, 5, '', '', $true)
      $document.ExportAsFixedFormat(0, $target)
    }
    'powerpoint_images' {
      $app = New-Object -ComObject PowerPoint.Application
      $app.DisplayAlerts = 1
      $app.AutomationSecurity = 3
      $document = $app.Presentations.Open($source, $true, $false, $false)
      $document.Export($target, 'PNG')
    }
    default { throw 'Unsupported Office enhancement kind' }
  }
}
finally {
  if ($null -ne $document) {
    try { $document.Close() } catch {}
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($document)
  }
  if ($null -ne $app) {
    try { $app.Quit() } catch {}
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($app)
  }
  [GC]::Collect()
  [GC]::WaitForPendingFinalizers()
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_enforces_read_only_macro_and_link_boundaries() {
        for marker in [
            "AutomationSecurity = 3",
            "AskToUpdateLinks = $false",
            "Workbooks.Open($source, 0, $true",
            "Presentations.Open($source, $true",
            "Documents.Open($source, $false, $true",
            "FinalReleaseComObject",
        ] {
            assert!(OFFICE_SCRIPT.contains(marker), "missing {marker}");
        }
        assert!(!OFFICE_SCRIPT.contains("SaveAs"));
        assert!(!OFFICE_SCRIPT.contains("$source = '"));
    }

    #[test]
    fn validation_rejects_kind_mismatch_and_original_target() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("book.docx");
        fs::write(&source, b"fixture").unwrap();
        let source = source.canonicalize().unwrap();
        let target = temp.path().canonicalize().unwrap().join("preview.pdf");
        let mut request =
            OfficeEnhanceRequest::new(OfficeEnhancementKind::ExcelPdf, source.clone(), target);
        assert!(validate_request(&request).is_err());
        request.kind = OfficeEnhancementKind::WordPdf;
        request.target = source;
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn cancellation_is_shareable() {
        let cancellation = OfficeCancellation::default();
        let second = cancellation.clone();
        cancellation.cancel();
        assert!(second.is_cancelled());
    }
}
