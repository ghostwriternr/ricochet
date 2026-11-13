use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("camera: {0}")]
    Camera(String),
    #[error("phone: {0}")]
    Phone(String),
    #[error("{} already exists with different contents", .0.display())]
    Conflict(PathBuf),
    #[error("cancelled")]
    Cancelled,
}
