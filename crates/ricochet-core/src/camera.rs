//! The camera, read-only: Ricochet lists and downloads, and never deletes, uploads or changes
//! settings on it.

use std::path::Path;

use crate::plan::CameraFile;
use crate::{Error, Result};

pub trait Camera {
    /// The model, as the camera reports it.
    fn name(&self) -> &str;
    /// Every file on every storage, in folder order.
    fn list(&mut self) -> Result<Vec<CameraFile>>;
    /// Downloads one file to `to`, which must not exist yet.
    fn download(&mut self, file: &CameraFile, to: &Path) -> Result<()>;
}

/// The first connected camera, or `None` when there isn't one.
pub fn detect() -> Result<Option<Box<dyn Camera + Send>>> {
    gphoto::detect()
}

mod gphoto {
    use super::*;
    use gphoto2::Context;

    pub struct Gphoto {
        context: Context,
        camera: gphoto2::Camera,
        name: String,
    }

    fn err(e: gphoto2::Error) -> Error {
        Error::Camera(e.to_string())
    }

    pub fn detect() -> Result<Option<Box<dyn Camera + Send>>> {
        let context = Context::new().map_err(err)?;
        // An iPhone also answers as a PTP camera; it's the phone, not a source.
        let mut cameras = context.list_cameras().wait().map_err(err)?;
        let Some(descriptor) = cameras.find(|c| !c.model.starts_with("Apple ")) else {
            return Ok(None);
        };
        let camera = context.get_camera(&descriptor).wait().map_err(err)?;
        Ok(Some(Box::new(Gphoto { context, camera, name: descriptor.model })))
    }

    // libgphoto2 handles are used from one thread at a time; Ricochet keeps each camera on the
    // worker that opened it, so moving it there once is sound.
    unsafe impl Send for Gphoto {}

    impl Gphoto {
        fn walk(&self, folder: &str, out: &mut Vec<CameraFile>) -> Result<()> {
            let fs = self.camera.fs();
            for name in fs.list_files(folder).wait().map_err(err)? {
                let info = fs.file_info(folder, &name).wait().map_err(err)?;
                let size = info.file().size().unwrap_or(0);
                out.push(CameraFile { folder: folder.to_owned(), name, size });
            }
            let mut subfolders: Vec<String> = fs.list_folders(folder).wait().map_err(err)?.collect();
            subfolders.sort();
            for sub in subfolders {
                let path = if folder == "/" { format!("/{sub}") } else { format!("{folder}/{sub}") };
                self.walk(&path, out)?;
            }
            Ok(())
        }
    }

    impl Camera for Gphoto {
        fn name(&self) -> &str {
            &self.name
        }

        fn list(&mut self) -> Result<Vec<CameraFile>> {
            let mut files = Vec::new();
            self.walk("/", &mut files)?;
            Ok(files)
        }

        fn download(&mut self, file: &CameraFile, to: &Path) -> Result<()> {
            let _ = &self.context;
            self.camera.fs().download_to(&file.folder, &file.name, to).wait().map_err(err)?;
            Ok(())
        }
    }
}
