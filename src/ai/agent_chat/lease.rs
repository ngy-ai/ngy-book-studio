//! The per-library conversation lease and the claim it reserves.

use super::*;

#[derive(Debug)]
pub(crate) struct ConversationLease {
    pub(crate) database_path: PathBuf,
    pub(crate) owner: u64,
    pub(crate) thread_id: Mutex<Option<String>>,
}

#[derive(Debug)]
pub(crate) struct ConversationClaimReservation {
    pub(crate) database_path: PathBuf,
    pub(crate) owner: u64,
    pub(crate) thread_id: String,
    pub(crate) temporary: bool,
}

impl ConversationLease {
    pub(super) fn new(database_path: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
            owner: NEXT_CONVERSATION_OWNER.fetch_add(1, Ordering::Relaxed),
            thread_id: Mutex::new(None),
        }
    }

    /// Claims `thread_id` and releases this lease's previous claim atomically.
    ///
    /// A failed switch leaves the previous claim untouched, so a window never
    /// loses its current conversation merely because another window already
    /// owns the requested one.
    pub(super) fn switch_claim(&self, thread_id: &str) -> Result<bool> {
        let mut claims = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation claim lock is poisoned"))?;
        let mut selected = self
            .thread_id
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation lease lock is poisoned"))?;
        let database_claims = claims.entry(self.database_path.clone()).or_default();
        match database_claims.get(thread_id) {
            Some(owner) if *owner != self.owner => return Ok(false),
            Some(_) => {}
            None => {
                database_claims.insert(thread_id.to_string(), self.owner);
            }
        }
        if let Some(previous) = selected.as_deref()
            && previous != thread_id
            && database_claims.get(previous) == Some(&self.owner)
        {
            database_claims.remove(previous);
        }
        *selected = Some(thread_id.to_string());
        Ok(true)
    }

    pub(super) fn clear_claim(&self) -> Result<()> {
        let mut claims = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation claim lock is poisoned"))?;
        let mut selected = self
            .thread_id
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation lease lock is poisoned"))?;
        if let Some(database_claims) = claims.get_mut(&self.database_path) {
            if let Some(thread_id) = selected.as_deref()
                && database_claims.get(thread_id) == Some(&self.owner)
            {
                database_claims.remove(thread_id);
            }
            if database_claims.is_empty() {
                claims.remove(&self.database_path);
            }
        }
        *selected = None;
        Ok(())
    }

    /// Temporarily claims `thread_id` without releasing this lease's selected
    /// conversation. Callers can inspect and authorize the target while every
    /// other window is excluded, then either promote or drop the reservation.
    /// A thread already claimed by this lease needs no additional registry
    /// entry.
    pub(super) fn reserve_claim(
        &self,
        thread_id: &str,
    ) -> Result<Option<ConversationClaimReservation>> {
        let mut claims = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation claim lock is poisoned"))?;
        let database_claims = claims.entry(self.database_path.clone()).or_default();
        let temporary = match database_claims.get(thread_id) {
            Some(owner) if *owner != self.owner => return Ok(None),
            Some(_) => false,
            None => {
                database_claims.insert(thread_id.to_string(), self.owner);
                true
            }
        };
        Ok(Some(ConversationClaimReservation {
            database_path: self.database_path.clone(),
            owner: self.owner,
            thread_id: thread_id.to_string(),
            temporary,
        }))
    }
}

impl ConversationClaimReservation {
    /// Promotes a temporary reservation to this lease's selected conversation.
    /// The old selected claim is released only after the caller has loaded and
    /// authorized the new thread while this reservation excludes deletion.
    pub(super) fn promote_to_selection(&mut self, lease: &ConversationLease) -> Result<()> {
        ensure!(
            self.database_path == lease.database_path && self.owner == lease.owner,
            "conversation reservation belongs to another window"
        );
        ensure!(
            lease.switch_claim(&self.thread_id)?,
            "conversation reservation was lost before selection"
        );
        self.temporary = false;
        Ok(())
    }
}

impl Drop for ConversationClaimReservation {
    fn drop(&mut self) {
        if !self.temporary {
            return;
        }
        let Ok(mut claims) = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
        else {
            return;
        };
        if let Some(database_claims) = claims.get_mut(&self.database_path) {
            if database_claims.get(&self.thread_id) == Some(&self.owner) {
                database_claims.remove(&self.thread_id);
            }
            if database_claims.is_empty() {
                claims.remove(&self.database_path);
            }
        }
    }
}

impl Drop for ConversationLease {
    fn drop(&mut self) {
        let Ok(thread_id) = self.thread_id.get_mut() else {
            return;
        };
        let Some(thread_id) = thread_id.take() else {
            return;
        };
        let Ok(mut claims) = THREAD_CLAIMS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
        else {
            return;
        };
        if let Some(database_claims) = claims.get_mut(&self.database_path) {
            if database_claims.get(&thread_id) == Some(&self.owner) {
                database_claims.remove(&thread_id);
            }
            if database_claims.is_empty() {
                claims.remove(&self.database_path);
            }
        }
    }
}
