pub mod event;
pub mod hooks;
pub mod storage;
pub mod transcript;

/// Shared test utilities.
#[cfg(test)]
pub mod test_support {
    use std::sync::Mutex;

    /// Process-wide lock to serialize tests that mutate the $HOME environment variable.
    pub static HOME_LOCK: Mutex<()> = Mutex::new(());
}
