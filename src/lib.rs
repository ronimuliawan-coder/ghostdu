pub mod fs;
pub mod ghost;
pub mod ops;
pub mod ui;

/// Serializes tests that mutate `XDG_DATA_HOME`. The variable is
/// process-global and several trash tests repoint it concurrently; without
/// the lock a trash op can read another test's value mid-flight.
#[cfg(test)]
pub(crate) static XDG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Holds `XDG_TEST_LOCK` and restores `XDG_DATA_HOME` on drop, so a failed
/// assertion can never leak a fixture-scoped value (or a poisoned lock
/// holder) into sibling tests.
#[cfg(test)]
pub(crate) struct XdgGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl XdgGuard {
    pub(crate) fn set(path: &std::path::Path) -> Self {
        // The () payload carries no invariant worth protecting: recover the
        // lock so one panicking test cannot cascade into unrelated failures.
        let lock = XDG_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", path) };
        Self { _lock: lock, saved }
    }
}

#[cfg(test)]
impl Drop for XdgGuard {
    fn drop(&mut self) {
        if let Some(ref saved) = self.saved {
            unsafe { std::env::set_var("XDG_DATA_HOME", saved) };
        } else {
            unsafe { std::env::remove_var("XDG_DATA_HOME") };
        }
    }
}
