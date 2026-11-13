//! Everything Ricochet does without a window: where photos live, what's new, and the copying.
//! The app and any future trigger (a udev rule, a launchd agent) drive the same functions.

pub mod camera;
mod error;
pub mod library;
pub mod phone;
pub mod plan;
pub mod transfer;

pub use error::{Error, Result};
pub use library::{Kind, Library, Photo};
