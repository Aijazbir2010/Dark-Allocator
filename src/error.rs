use std::error::Error;
use std::fmt::Display;

#[derive(Debug)]
pub enum DarkError {
    AllocationError(String),
    OutOfMemory,
    SysOutOfMemory
}

impl Display for DarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DarkError::AllocationError(s) => write!(f, "{s}"),
            DarkError::OutOfMemory => write!(f, "Not enough memory available for the allocation."),
            DarkError::SysOutOfMemory => write!(f, "All available system memory has been used up."),
        }
    }
}

impl Error for DarkError {}