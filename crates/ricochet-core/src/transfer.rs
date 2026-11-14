//! Running a plan: one file at a time, reporting progress, stopping between files when asked.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::camera::Camera;
use crate::phone::Phone;
use crate::plan::CameraFile;
use crate::{Error, Library, Photo, Result};

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A file started; `index` counts from 0.
    File { index: usize, name: String },
    /// Bytes done across the whole run so far.
    Bytes(u64),
}

/// Downloads `todo` from the camera and files each into the library.
pub fn import(
    camera: &mut dyn Camera,
    library: &Library,
    todo: &[CameraFile],
    cancel: &AtomicBool,
    on: &mut dyn FnMut(Event),
) -> Result<Vec<Photo>> {
    let incoming = library.incoming();
    fs::create_dir_all(&incoming)?;
    let mut done = 0;
    let mut filed = Vec::new();
    for (index, file) in todo.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        on(Event::File { index, name: file.name.clone() });
        let staged = incoming.join(&file.name);
        let _ = fs::remove_file(&staged);
        camera.download(file, &staged, &mut |got| on(Event::Bytes(done + got)))?;
        let got = fs::metadata(&staged)?.len();
        if file.size != 0 && got != file.size {
            let _ = fs::remove_file(&staged);
            return Err(Error::Camera(format!("{}: got {got} of {} bytes", file.name, file.size)));
        }
        filed.push(library.file(&staged, &file.name)?);
        done += got;
        on(Event::Bytes(done));
    }
    let _ = fs::remove_dir(&incoming);
    Ok(filed)
}

/// Copies `todo` from the library to the phone.
pub fn push(
    phone: &mut dyn Phone,
    library: &Library,
    todo: &[Photo],
    cancel: &AtomicBool,
    on: &mut dyn FnMut(Event),
) -> Result<()> {
    let mut done = 0;
    for (index, photo) in todo.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        on(Event::File { index, name: photo.name.clone() });
        phone.upload(&library.path(photo), photo, &mut |sent| on(Event::Bytes(done + sent)))?;
        done += photo.size;
    }
    Ok(())
}
