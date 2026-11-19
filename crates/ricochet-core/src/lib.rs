//! Everything Ricochet does without a window: where photos live, what's new, and the copying.
//! The app and any future trigger (a udev rule, a launchd agent) drive the same functions.

pub mod camera;
pub mod db;
mod error;
pub mod library;
pub mod phone;
pub mod plan;
pub mod sync;

pub use db::Db;
pub use error::{Error, Result};
pub use library::{Kind, Library, Photo};
