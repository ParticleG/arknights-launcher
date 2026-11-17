use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub enum WorkError {
    Cancelled,
    Failed(String),
}

impl fmt::Display for WorkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Operation cancelled"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl From<String> for WorkError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<std::io::Error> for WorkError {
    fn from(error: std::io::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

#[derive(Default)]
pub struct Control {
    pub cancelled: AtomicBool,
    pub stopped: AtomicBool,
}

impl Control {
    pub fn check(&self) -> Result<(), WorkError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(WorkError::Cancelled)
        } else if self.stopped.load(Ordering::Acquire) {
            Err(WorkError::Failed("Another installation worker failed".into()))
        } else {
            Ok(())
        }
    }

    pub fn error(&self, error: impl fmt::Display) -> WorkError {
        if self.cancelled.load(Ordering::Acquire) {
            WorkError::Cancelled
        } else {
            WorkError::Failed(error.to_string())
        }
    }
}

impl std::error::Error for WorkError {}
