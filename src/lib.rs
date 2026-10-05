pub mod fs;
pub mod ghost;
pub mod ops;
pub mod ui;

/// Serializes tests that mutate `XDG_DATA_HOME`. The variable is
/// process-global and several trash tests repoint it concurrently; without
/// the lock a trash op can read another test's value mid-flight.
#[cfg(test)]
pub(crate) static XDG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
