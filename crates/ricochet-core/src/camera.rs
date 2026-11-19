//! The camera, read-only: Ricochet lists and downloads, and never deletes, uploads or changes
//! settings on it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::sync_channel;

use bytes::Bytes;
use futures_executor::block_on;
use mtp_rs::mtp::DEFAULT_CANCEL_TIMEOUT;
use mtp_rs::ptp::{ObjectHandle, OperationCode, PtpDevice, PtpError, PtpSession, StorageId};

use crate::db::Db;
use crate::plan::CameraFile;
use crate::{Error, Result};

/// One object as the camera describes it: a file or a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    pub storage: u32,
    pub name: String,
    /// 0 when the camera can't say, as for files over 4 GB.
    pub size: u64,
    /// When it was made, as the camera says; empty when it doesn't.
    pub created: String,
    pub folder: bool,
}

/// The few PTP operations listing and copying need. Handles stay valid across sessions on the
/// GR IV, which is what lets [`list`] remember descriptions between runs.
pub trait Camera {
    /// The model, as the camera reports it.
    fn name(&self) -> &str;
    /// Tells this camera apart from others with the same name.
    fn serial(&self) -> &str;
    fn storages(&mut self) -> Result<Vec<u32>>;
    /// The handles PTP's GetObjectHandles gives for `parent` on `storage`.
    fn children(&mut self, storage: u32, parent: u32) -> Result<Vec<u32>>;
    /// `None` when the camera won't describe this object.
    fn describe(&mut self, handle: u32) -> Result<Option<Object>>;
    /// Downloads one file to `to`, which must not exist yet, reporting bytes done so far.
    fn download(&mut self, file: &CameraFile, to: &Path, progress: &mut dyn FnMut(u64)) -> Result<()>;
}

/// The first connected camera, or `None` when there isn't one.
pub fn detect() -> Result<Option<Box<dyn Camera + Send>>> {
    Ptp::detect()
}

/// PTP's GetObjectHandles parent for "every object on the storage", and for "the root".
const ALL: u32 = 0;
const ROOT: u32 = 0xFFFF_FFFF;

/// How many remembered descriptions are checked against the camera on each listing.
const SPOT_CHECKS: usize = 8;

/// Every file on the camera, by name, reporting objects described so far and how many have been
/// found (which grows as folders open).
///
/// The camera describes one object per request, about 14 ms each (31 s for 2,162 files), so
/// descriptions are remembered in `db` and only new handles are described. The memory is
/// dropped, and everything described afresh, when a remembered handle is gone (files were
/// deleted, or the card formatted, after which handles may name new files) or when a spot check
/// of remembered files disagrees with the camera.
pub fn list(camera: &mut dyn Camera, db: &Db, progress: &mut dyn FnMut(usize, usize)) -> Result<Vec<CameraFile>> {
    let serial = camera.serial().to_owned();
    let remembered = db.camera_objects(&serial)?;
    let objects = match walk(camera, &remembered, progress)? {
        Some(objects) => objects,
        None => walk(camera, &HashMap::new(), progress)?.expect("nothing remembered to disagree with"),
    };
    db.set_camera_objects(&serial, &objects)?;
    let mut files: Vec<CameraFile> = objects
        .into_iter()
        .filter(|(_, o)| !o.folder)
        .map(|(id, o)| CameraFile { id, name: o.name, size: o.size })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

/// Every object on every storage, using `remembered` descriptions where it can; `None` when
/// they turn out stale.
fn walk(
    camera: &mut dyn Camera,
    remembered: &HashMap<u32, Object>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Option<HashMap<u32, Object>>> {
    let mut objects = HashMap::new();
    let (mut seen, mut done) = (HashSet::new(), 0);
    for storage in camera.storages()? {
        // Cameras differ on which of the two they honour: the GR IV gives everything for ALL and
        // just DCIM for ROOT. Asking both and opening every folder finds everything either way.
        let mut parents = vec![ALL, ROOT];
        while let Some(parent) = parents.pop() {
            for handle in camera.children(storage, parent)? {
                if !seen.insert(handle) {
                    continue;
                }
                let object = match remembered.get(&handle) {
                    Some(o) if o.storage == storage => Some(o.clone()),
                    _ => camera.describe(handle)?,
                };
                done += 1;
                progress(done, seen.len());
                // An object the camera won't describe stays on the camera, untouched.
                let Some(object) = object else { continue };
                if object.folder {
                    parents.push(handle);
                }
                objects.insert(handle, object);
            }
        }
    }
    if remembered.keys().any(|h| !objects.contains_key(h)) {
        return Ok(None);
    }
    // The newest remembered files, and some spread across the rest.
    let mut handles: Vec<u32> = remembered.keys().copied().collect();
    handles.sort_unstable_by(|a, b| b.cmp(a));
    let step = (handles.len() / (SPOT_CHECKS / 2)).max(1);
    let checks: BTreeSet<u32> =
        handles.iter().take(SPOT_CHECKS / 2).chain(handles.iter().step_by(step).take(SPOT_CHECKS / 2)).copied().collect();
    for handle in checks {
        if camera.describe(handle)?.as_ref() != remembered.get(&handle) {
            return Ok(None);
        }
    }
    Ok(Some(objects))
}

const APPLE: u16 = 0x05ac;

fn err(e: PtpError) -> Error {
    Error::Camera(e.to_string())
}

/// mtp-rs's PTP layer. Its higher-level `MtpDevice` describes every object while listing and
/// again before each download; Ricochet already has the descriptions.
struct Ptp {
    name: String,
    serial: String,
    session: Arc<PtpSession>,
}

impl Ptp {
    fn detect() -> Result<Option<Box<dyn Camera + Send>>> {
        // An iPhone also has a PTP interface; it's the phone, not a source.
        let devices = PtpDevice::list_devices().map_err(|e| Error::Camera(e.to_string()))?;
        let Some(info) = devices.into_iter().find(|d| d.vendor_id != APPLE) else { return Ok(None) };
        let device = block_on(PtpDevice::open_by_location(info.location_id)).map_err(err)?;
        let about = block_on(device.get_device_info()).map_err(err)?;
        let session = Arc::new(block_on(device.open_session()).map_err(err)?);
        let name = info.product.unwrap_or(about.model);
        Ok(Some(Box::new(Ptp { name, serial: about.serial_number, session })))
    }
}

impl Camera for Ptp {
    fn name(&self) -> &str {
        &self.name
    }

    fn serial(&self) -> &str {
        &self.serial
    }

    fn storages(&mut self) -> Result<Vec<u32>> {
        let ids = block_on(self.session.get_storage_ids()).map_err(err)?;
        Ok(ids.into_iter().map(|id| id.0).collect())
    }

    fn children(&mut self, storage: u32, parent: u32) -> Result<Vec<u32>> {
        let handles = block_on(self.session.get_object_handles(StorageId(storage), None, Some(ObjectHandle(parent))))
            .map_err(err)?;
        Ok(handles.into_iter().map(|h| h.0).collect())
    }

    fn describe(&mut self, handle: u32) -> Result<Option<Object>> {
        let info = match block_on(self.session.get_object_info(ObjectHandle(handle))) {
            Ok(info) => info,
            Err(e) if e.response_code().is_some() => return Ok(None),
            Err(e) => return Err(err(e)),
        };
        let created = info.created.map(|d| {
            format!("{:04}{:02}{:02}T{:02}{:02}{:02}", d.year, d.month, d.day, d.hour, d.minute, d.second)
        });
        Ok(Some(Object {
            storage: info.storage_id.0,
            folder: info.is_folder(),
            name: info.filename,
            // PTP sizes top out at 4 GB, and the GR IV has no MTP property to ask for more, so a
            // longer clip's size is unknown.
            size: if info.size >= u32::MAX as u64 { 0 } else { info.size },
            created: created.unwrap_or_default(),
        }))
    }

    fn download(&mut self, file: &CameraFile, to: &Path, progress: &mut dyn FnMut(u64)) -> Result<()> {
        let out = File::create_new(to)?;
        // Writing happens on its own thread so the camera's next chunk is read meanwhile: 85 MB/s
        // with writes in line, against 95 for reads alone.
        let (chunks, received) = sync_channel::<Bytes>(64);
        std::thread::scope(|s| {
            let writer = s.spawn(move || -> std::io::Result<()> {
                let mut out = BufWriter::with_capacity(1 << 20, out);
                for chunk in received {
                    out.write_all(&chunk)?;
                }
                out.into_inner().map_err(|e| e.into_error())?.sync_all()
            });
            let read = block_on(async {
                let id = ObjectHandle(file.id);
                let mut stream = match file.size {
                    0 => self.session.execute_with_receive_stream(OperationCode::GetObject, &[id.0]).await,
                    size => {
                        self.session.execute_with_receive_stream_sized(OperationCode::GetObject, &[id.0], size).await
                    }
                }
                .map_err(err)?;
                let mut done = 0;
                while let Some(chunk) = stream.next_chunk().await {
                    let chunk = chunk.map_err(err)?;
                    done += chunk.len() as u64;
                    if chunks.send(chunk).is_err() {
                        // The writer failed, and its error says why. The camera is still sending,
                        // so end the transfer to leave the session usable.
                        return stream.cancel(DEFAULT_CANCEL_TIMEOUT).await.map_err(err);
                    }
                    progress(done);
                }
                Ok::<_, Error>(())
            });
            drop(chunks);
            let wrote = writer.join().expect("the writer doesn't panic");
            read?;
            Ok(wrote?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera with one storage, where ALL lists everything and ROOT lists folders, counting
    /// descriptions.
    #[derive(Default)]
    struct Fake {
        objects: HashMap<u32, Object>,
        described: usize,
    }

    impl Fake {
        fn with_files(names: &[(u32, &str, u64)]) -> Fake {
            let mut objects = HashMap::from([(1, object("DCIM", 0, true))]);
            for &(handle, name, size) in names {
                objects.insert(handle, object(name, size, false));
            }
            Fake { objects, described: 0 }
        }
    }

    fn object(name: &str, size: u64, folder: bool) -> Object {
        Object { storage: 1, name: name.into(), size, created: String::new(), folder }
    }

    impl Camera for Fake {
        fn name(&self) -> &str {
            "Fake"
        }
        fn serial(&self) -> &str {
            "1"
        }
        fn storages(&mut self) -> Result<Vec<u32>> {
            Ok(vec![1])
        }
        fn children(&mut self, _: u32, parent: u32) -> Result<Vec<u32>> {
            let mut handles: Vec<u32> = match parent {
                ALL => self.objects.keys().copied().collect(),
                _ => self.objects.iter().filter(|(_, o)| o.folder).map(|(h, _)| *h).collect(),
            };
            handles.sort();
            Ok(handles)
        }
        fn describe(&mut self, handle: u32) -> Result<Option<Object>> {
            self.described += 1;
            Ok(self.objects.get(&handle).cloned())
        }
        fn download(&mut self, _: &CameraFile, _: &Path, _: &mut dyn FnMut(u64)) -> Result<()> {
            unreachable!()
        }
    }

    fn names(files: &[CameraFile]) -> Vec<&str> {
        files.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn a_second_listing_describes_only_new_files_and_spot_checks() {
        let db = Db::in_memory().unwrap();
        let mut cam = Fake::with_files(&[(10, "R1.DNG", 5), (11, "R1.JPG", 3)]);
        assert_eq!(names(&list(&mut cam, &db, &mut |_, _| {}).unwrap()), ["R1.DNG", "R1.JPG"]);
        assert_eq!(cam.described, 3);

        cam.objects.insert(12, object("R2.DNG", 5, false));
        cam.described = 0;
        assert_eq!(names(&list(&mut cam, &db, &mut |_, _| {}).unwrap()), ["R1.DNG", "R1.JPG", "R2.DNG"]);
        // R2, then a spot check of each of the three remembered objects.
        assert_eq!(cam.described, 1 + 3);
    }

    #[test]
    fn a_vanished_handle_or_a_changed_file_means_describing_everything_again() {
        let db = Db::in_memory().unwrap();
        let mut cam = Fake::with_files(&[(10, "R1.DNG", 5), (11, "R2.DNG", 5)]);
        list(&mut cam, &db, &mut |_, _| {}).unwrap();

        // A format, then one new photo under a handle the old card used.
        cam.objects.remove(&11);
        cam.objects.insert(10, object("R1.DNG", 9, false));
        cam.described = 0;
        let files = list(&mut cam, &db, &mut |_, _| {}).unwrap();
        assert_eq!(files.iter().map(|f| (f.name.as_str(), f.size)).collect::<Vec<_>>(), [("R1.DNG", 9)]);
        assert_eq!(db.camera_objects("1").unwrap()[&10].size, 9);

        // Same handles, one file changed: a spot check catches it.
        cam.objects.insert(10, object("R1.DNG", 7, false));
        assert_eq!(list(&mut cam, &db, &mut |_, _| {}).unwrap()[0].size, 7);
    }
}
