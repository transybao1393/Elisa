//! A folder under the system temp dir for one test, removed when it goes out of scope: also when the test fails (the
//! drop runs while the panic unwinds), and with whatever a child process left in it. `new` keeps the path as the
//! system gives it; `real` resolves symlinks (macOS: /var → /private/var) for tests that compare paths with what the
//! code under test resolved. Shared by the integration tests (`mod common;`) and the library's own tests
//! (`crate::testdir`, src/lib.rs).
#![allow(dead_code)]
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// `elisa-<tag>-<pid>-<n>`, empty; a leftover of the same name (a killed run that had this pid) goes first
    pub fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("elisa-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }

    /// the same, as its real path (symlinks resolved)
    pub fn real(tag: &str) -> TempDir {
        let mut t = TempDir::new(tag);
        t.0 = t.0.canonicalize().unwrap();
        t
    }

    pub fn path(&self) -> &Path { &self.0 }
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path { &self.0 }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path { &self.0 }
}

/// a `Command` argument or environment value
impl AsRef<std::ffi::OsStr> for TempDir {
    fn as_ref(&self) -> &std::ffi::OsStr { self.0.as_os_str() }
}

impl Drop for TempDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}
