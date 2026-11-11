pub mod agents;
pub mod app;
pub mod applescript;
pub mod bridges;
pub mod capture;
pub mod distribution;
pub mod files;
pub mod herd;
pub mod install;
pub mod lifecycle;
pub mod macos;
pub mod native;
pub mod process;
pub mod sessions;
pub mod ui;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub fn invalid(message: impl Into<String>) -> Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()).into()
}
