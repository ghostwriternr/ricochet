//! The camera, read-only: Ricochet lists and downloads, and never deletes, uploads or changes
//! settings on it.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use futures_executor::block_on;
use mtp_rs::{ByteRange, DownloadPacing, ListingItem, MtpDevice, ObjectHandle, Storage, StorageId};

use crate::plan::CameraFile;
use crate::{Error, Result};

pub trait Camera {
    /// The model, as the camera reports it.
    fn name(&self) -> &str;
    /// Every file on every storage, by name, reporting files read so far and how many there are
    /// in the folders seen so far (which grows as the walk goes).
    fn list(&mut self, progress: &mut dyn FnMut(usize, usize)) -> Result<Vec<CameraFile>>;
    /// Downloads one file to `to`, which must not exist yet, reporting bytes done so far.
    fn download(&mut self, file: &CameraFile, to: &Path, progress: &mut dyn FnMut(u64)) -> Result<()>;
}

/// The first connected camera, or `None` when there isn't one.
pub fn detect() -> Result<Option<Box<dyn Camera + Send>>> {
    Ptp::detect()
}

const APPLE: u16 = 0x05ac;

fn err(e: mtp_rs::Error) -> Error {
    Error::Camera(e.to_string())
}

struct Ptp {
    name: String,
    device: MtpDevice,
    storages: Vec<Storage>,
}

impl Ptp {
    fn detect() -> Result<Option<Box<dyn Camera + Send>>> {
        // An iPhone also has a PTP interface; it's the phone, not a source.
        let devices = MtpDevice::list_devices().map_err(err)?;
        let Some(info) = devices.into_iter().find(|d| d.vendor_id != APPLE) else { return Ok(None) };
        let device = block_on(MtpDevice::open_by_location(info.location_id)).map_err(err)?;
        let name = info.product.unwrap_or_else(|| device.device_info().model.clone());
        Ok(Some(Box::new(Ptp { name, device, storages: Vec::new() })))
    }

    fn storage(&self, id: u64) -> Result<&Storage> {
        let found = self.storages.iter().find(|s| s.id() == StorageId(id));
        found.ok_or_else(|| Error::Camera("the camera's storage went away".into()))
    }
}

impl Camera for Ptp {
    fn name(&self) -> &str {
        &self.name
    }

    fn list(&mut self, progress: &mut dyn FnMut(usize, usize)) -> Result<Vec<CameraFile>> {
        self.storages = block_on(self.device.storages()).map_err(err)?;
        let (mut files, mut read, mut total) = (Vec::new(), 0, 0);
        for storage in &self.storages {
            // The camera describes one object per request, about 14 ms each, so this is where
            // the time goes. Walking a folder at a time lets the count show.
            let mut folders = vec![None];
            while let Some(folder) = folders.pop() {
                let mut listing = block_on(storage.list_objects_stream(folder)).map_err(err)?;
                total += listing.total();
                while let Some(item) = block_on(listing.next()) {
                    read += 1;
                    progress(read, total);
                    // An object the camera won't describe stays on the camera, untouched.
                    let ListingItem::Object(object) = item.map_err(err)? else { continue };
                    if object.is_folder() {
                        folders.push(Some(object.handle));
                        continue;
                    }
                    // PTP sizes top out at 4 GB, and the GR IV has no MTP property to ask for
                    // more, so a longer clip's size is unknown.
                    let size = if object.size == u32::MAX as u64 { 0 } else { object.size };
                    files.push(CameraFile { storage: storage.id().0, id: object.handle.0, name: object.filename, size });
                }
            }
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(files)
    }

    fn download(&mut self, file: &CameraFile, to: &Path, progress: &mut dyn FnMut(u64)) -> Result<()> {
        let storage = self.storage(file.storage)?;
        let mut out = BufWriter::with_capacity(1 << 20, File::create_new(to)?);
        block_on(async {
            // One transfer per file is about 15% faster than mtp-rs's default windows, which
            // exist to let other calls in between; nothing else uses the camera meanwhile, and
            // Stop waits for the current file anyway.
            let mut download = storage
                .download_with_pacing(ObjectHandle(file.id), ByteRange::Full, DownloadPacing::HeldOpen)
                .await
                .map_err(err)?;
            let mut done = 0;
            while let Some(chunk) = download.next_chunk().await {
                let chunk = chunk.map_err(err)?;
                out.write_all(&chunk)?;
                done += chunk.len() as u64;
                progress(done);
            }
            Ok::<_, Error>(())
        })?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    }
}
