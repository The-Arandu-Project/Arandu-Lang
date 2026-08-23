//! Text-edit VFS used by the LSP (and any host that streams full-document text).
//!
//! Built on [`crate::debounce::DebouncedMap`] so CLI watch and LSP share one
//! debounce implementation — not two parallel buffers.

use crate::debounce::DebouncedMap;
use std::time::Duration;

pub use crate::debounce::DEFAULT_DEBOUNCE as EDIT_VFS_DEFAULT_DEBOUNCE;

/// Pending full-document texts keyed by URI (or absolute path string).
#[derive(Debug)]
pub struct EditVfs {
    inner: DebouncedMap<String, String>,
}

impl Default for EditVfs {
    fn default() -> Self {
        Self::new()
    }
}

impl EditVfs {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: DebouncedMap::new(),
        }
    }

    #[must_use]
    pub fn with_debounce(debounce: Duration) -> Self {
        Self {
            inner: DebouncedMap::with_debounce(debounce),
        }
    }

    /// Queue a full-document replace (LSP `TextDocumentSyncKind::FULL`).
    pub fn push_full_text(&mut self, uri: String, text: String) {
        self.inner.push(uri, text);
    }

    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.inner.has_pending()
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        self.inner.next_deadline()
    }

    pub fn take_due(&mut self) -> Vec<(String, String)> {
        self.inner.take_due()
    }

    pub fn take_all(&mut self) -> Vec<(String, String)> {
        self.inner.take_all()
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.inner.pending_count()
    }

    #[must_use]
    pub fn pending_text(&self, uri: &str) -> Option<&str> {
        self.inner.get(uri).map(String::as_str)
    }

    #[must_use]
    pub fn debounce(&self) -> Duration {
        self.inner.debounce()
    }
}

// Keep the LSP-facing name available without a second type.
pub type Vfs = EditVfs;

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn debounce_batches_multiple_pushes() {
        let mut vfs = EditVfs::with_debounce(Duration::from_millis(40));
        vfs.push_full_text("file:///a.aru".into(), "v1".into());
        vfs.push_full_text("file:///a.aru".into(), "v2".into());
        vfs.push_full_text("file:///a.aru".into(), "v3".into());
        assert_eq!(vfs.pending_count(), 1);
        assert!(vfs.take_due().is_empty());
        thread::sleep(Duration::from_millis(50));
        let due = vfs.take_due();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].1, "v3");
    }

    #[test]
    fn pending_text_tracks_the_coalesced_value() {
        let mut vfs = EditVfs::new();
        vfs.push_full_text("file:///a.aru".into(), "v1".into());
        assert_eq!(vfs.pending_text("file:///a.aru"), Some("v1"));

        vfs.push_full_text("file:///a.aru".into(), "v2".into());
        assert_eq!(vfs.pending_text("file:///a.aru"), Some("v2"));
        assert_eq!(vfs.pending_text("file:///missing.aru"), None);
    }
}
