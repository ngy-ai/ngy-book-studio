//! The on-disk record of one chapter: atomic writes, export and restore.

use super::*;

pub fn new_run_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).context("无法生成学习记录标识")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub(crate) fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

impl LearningStore {
    pub fn prepare_directory(&self) -> Result<()> {
        fs::create_dir_all(self.directory.join("archives")).context("无法创建学习档案目录")
    }

    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            chapter: 1,
        }
    }

    pub fn for_chapter(directory: PathBuf, chapter: u8) -> Result<Self> {
        validate_chapter(chapter)?;
        Ok(Self { directory, chapter })
    }

    pub fn chapter(&self) -> u8 {
        self.chapter
    }

    pub fn course_id(&self) -> &'static str {
        chapter_course_id(self.chapter)
    }

    pub(crate) fn record_path(&self) -> PathBuf {
        self.directory
            .join(format!("chapter-{:02}.json", self.chapter))
    }

    pub(crate) fn load(&self) -> Result<RecordPayload> {
        let path = self.record_path();
        if !path.try_exists().context("无法检查学习记录")? {
            return Ok(RecordPayload {
                course_id: self.course_id().into(),
                course_version: COURSE_VERSION.into(),
                workspace: LearningWorkspace::for_chapter(self.chapter)?,
                attempts: vec![],
            });
        }
        decode_for(&read_bounded(&path)?, self.chapter)
    }

    pub(crate) fn write(&self, payload: &RecordPayload) -> Result<()> {
        fs::create_dir_all(&self.directory).context("无法创建学习记录目录")?;
        atomic_write(&self.record_path(), &encode(payload)?)
    }

    pub fn workspace(&self) -> Result<LearningWorkspace> {
        Ok(self.load()?.workspace)
    }

    pub fn save(&self, mut workspace: LearningWorkspace) -> Result<LearningWorkspace> {
        workspace.validate_for(self.chapter)?;
        let mut payload = self.load()?;
        ensure!(
            workspace.revision == payload.workspace.revision,
            "学习记录已更新，请重新载入后再保存"
        );
        workspace.revision = workspace
            .revision
            .checked_add(1)
            .context("学习记录版本溢出")?;
        payload.workspace = workspace.clone();
        self.write(&payload)?;
        Ok(workspace)
    }

    /// Save an immutable first attempt before launching any student code.
    pub fn begin(&self, workspace: LearningWorkspace, id: &str) -> Result<LearningWorkspace> {
        workspace.validate_for(self.chapter)?;
        ensure!(valid_id(id), "运行标识无效");
        let mut payload = self.load()?;
        ensure!(
            workspace.revision == payload.workspace.revision,
            "学习记录已更新，请重新载入"
        );
        ensure!(
            payload.attempts.len() < MAX_ATTEMPTS,
            "已有 64 次运行记录；请点击新一轮，归档已有证据后继续练习"
        );
        ensure!(
            !payload.attempts.iter().any(|attempt| attempt.id == id),
            "运行标识重复"
        );
        let mut saved = workspace;
        saved.revision = saved.revision.checked_add(1).context("学习记录版本溢出")?;
        payload.workspace = saved.clone();
        payload.attempts.push(StoredAttempt {
            id: id.into(), created_at: now(), code_digest: blake3::hash(saved.code().as_bytes()).to_hex().to_string(),
            workspace: saved.clone(), imported: false,
            report: serde_json::json!({"run_id":id,"request":{"chapter":self.chapter,"course_id":self.course_id(),"course_version":COURSE_VERSION,"rules_version":"1.0.0","task_version":"1.0.0","implementation":saved.implementation,"task_id":saved.scenario,"model_mode":"scripted"},"outcome":{"status":"interrupted","stop_reason":"run_not_committed","answer":null},"passed":false,"metrics":{},"observations":{},"learning":{"status":"pending_review"}}),
        });
        self.write(&payload)?;
        Ok(saved)
    }

    pub fn finish(&self, id: &str, report: Value) -> Result<LearningRunReport> {
        ensure!(
            serde_json::to_vec(&report)?.len() <= MAX_REPORT_BYTES,
            "运行报告超过 2 MiB"
        );
        ensure!(
            report.get("run_id").and_then(Value::as_str) == Some(id),
            "运行报告标识不匹配"
        );
        let mut payload = self.load()?;
        let attempt = payload
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == id)
            .context("运行记录不存在")?;
        ensure!(
            attempt
                .report
                .pointer("/outcome/stop_reason")
                .and_then(Value::as_str)
                == Some("run_not_committed"),
            "已经完成的运行证据不能被覆盖"
        );
        validate_attempt_identity(&report, &attempt.workspace, self.chapter, false)?;
        attempt.report = report;
        let result = present(attempt);
        self.write(&payload)?;
        Ok(result)
    }

    pub fn history(&self) -> Result<Vec<LearningRunSummary>> {
        Ok(self
            .load()?
            .attempts
            .iter()
            .rev()
            .map(|attempt| {
                let text = |key: &str| {
                    attempt
                        .report
                        .pointer(key)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                LearningRunSummary {
                    id: attempt.id.clone(),
                    created_at: attempt.created_at.clone(),
                    status: text("/outcome/status"),
                    stop_reason: text("/outcome/stop_reason"),
                    passed: attempt
                        .report
                        .get("passed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    imported: attempt.imported,
                    implementation: attempt.workspace.implementation.clone(),
                    scenario: attempt.workspace.scenario.clone(),
                }
            })
            .collect())
    }

    pub fn report(&self, id: &str) -> Result<LearningRunReport> {
        ensure!(valid_id(id), "运行标识无效");
        let payload = self.load()?;
        Ok(present(
            payload
                .attempts
                .iter()
                .find(|attempt| attempt.id == id)
                .context("运行记录不存在")?,
        ))
    }

    pub fn export(&self, path: &Path) -> Result<()> {
        let payload = self.load()?;
        self.check_export_destination(path)?;
        atomic_write(path, &encode(&payload)?)
    }

    pub(crate) fn check_export_destination(&self, path: &Path) -> Result<()> {
        // A backup must not overwrite any chapter's live record, including a
        // differently spelled Windows path to the same data directory.
        if let Ok(directory) = fs::canonicalize(&self.directory) {
            let parent = path.parent().context("学习备份路径无效")?;
            let parent = fs::canonicalize(parent).context("无法定位学习备份目录")?;
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            ensure!(
                parent != directory
                    || !(1..=10).any(|chapter| filename
                        .eq_ignore_ascii_case(&format!("chapter-{chapter:02}.json"))),
                "请导出到新的备份文件，不能覆盖任何章节的当前档案"
            );
            if let Ok(target) = fs::canonicalize(path) {
                for chapter in 1..=10 {
                    ensure!(
                        fs::canonicalize(self.directory.join(format!("chapter-{chapter:02}.json")))
                            .ok()
                            .as_ref()
                            != Some(&target),
                        "请导出到新的备份文件，不能覆盖任何章节的当前档案"
                    );
                }
            }
        }
        Ok(())
    }

    pub fn restore(&self, path: &Path) -> Result<()> {
        let mut imported = decode_for(&read_bounded(path)?, self.chapter)?;
        // A valid backup must also be usable when the current envelope is corrupt.
        // Keep its exact bytes for diagnosis before publishing any replacement.
        let current_path = self.record_path();
        let current_bytes = if current_path.try_exists().context("无法检查当前学习档案")?
        {
            read_bounded(&current_path)?
        } else {
            encode(&self.load()?)?
        };
        let current_revision = decode_for(&current_bytes, self.chapter)
            .map(|current| current.workspace.revision)
            .unwrap_or(imported.workspace.revision);
        for attempt in &mut imported.attempts {
            attempt.imported = true;
        }
        imported.workspace.revision = current_revision
            .checked_add(1)
            .context("学习记录版本溢出")?;
        // Archive the current first attempts before publishing a restored workbook.
        fs::create_dir_all(self.directory.join("archives")).context("无法创建学习档案目录")?;
        let backup = self
            .directory
            .join("archives")
            .join(format!("{}.json", new_run_id()?));
        atomic_write(&backup, &current_bytes)?;
        self.write(&imported)
    }

    /// Preserve prior evidence in a restorable archive; keep the current work.
    pub fn new_round(&self, mut workspace: LearningWorkspace) -> Result<()> {
        workspace.validate_for(self.chapter)?;
        let mut payload = self.load()?;
        ensure!(
            workspace.revision == payload.workspace.revision,
            "学习记录已更新，请重新载入后再开始新一轮"
        );
        fs::create_dir_all(self.directory.join("archives")).context("无法创建学习档案目录")?;
        atomic_write(
            &self
                .directory
                .join("archives")
                .join(format!("{}.json", new_run_id()?)),
            &encode(&payload)?,
        )?;
        payload.attempts.clear();
        workspace.revision = workspace
            .revision
            .checked_add(1)
            .context("学习记录版本溢出")?;
        payload.workspace = workspace;
        self.write(&payload)
    }
}
