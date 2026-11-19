//! A sync: the camera imports into the library while the phone gets what the library has, both
//! at once. Each side stops between files when asked, and one failing doesn't stop the other.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::Mutex;

use crate::camera::{self, Camera};
use crate::db::Db;
use crate::phone::{self, Phone, Target, Uploader};
use crate::plan::{self, CameraFile, CameraPlan, PhonePlan};
use crate::{Error, Library, Photo, Result};

/// Connections the phone uploads over at once. Over USB 2, two ran at 30.5 MB/s against 27.5 for
/// one, and four or eight were slower than two.
const UPLOADERS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Camera,
    Phone,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A file started; `index` counts from 0 in the order files start.
    File { index: usize, name: String },
    /// Bytes done on this side so far.
    Bytes(u64),
}

/// Whether to copy again what was copied before and since deleted at the destination.
#[derive(Clone, Copy, Debug, Default)]
pub struct Restore {
    pub camera: bool,
    pub phone: bool,
}

/// A connected camera and its files.
pub struct CameraScan {
    pub camera: Box<dyn Camera + Send>,
    pub files: Vec<CameraFile>,
}

/// A connected phone and the photos it has, by path.
pub struct PhoneScan {
    pub phone: Box<dyn Phone + Send>,
    pub photos: HashMap<String, u64>,
}

/// Lists the camera, using and updating what `db` remembers of it.
pub fn scan_camera(
    mut camera: Box<dyn Camera + Send>,
    db: &Db,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<CameraScan> {
    let files = camera::list(camera.as_mut(), db, progress)?;
    Ok(CameraScan { camera, files })
}

/// Lists the phone, trusting the sizes `db` recorded for what Ricochet sent.
pub fn scan_phone(mut phone: Box<dyn Phone + Send>, db: &Db) -> Result<PhoneScan> {
    let sent = db.sent(phone.id())?;
    let photos = phone.list(&sent)?.into_iter().map(|p| (p.rel_path(), p.size)).collect();
    Ok(PhoneScan { phone, photos })
}

/// Both devices, each `None` when it isn't connected.
pub struct Scan {
    pub camera: Result<Option<CameraScan>>,
    pub phone: Result<Option<PhoneScan>>,
}

/// Finds and lists both devices at once, reporting the camera's name and listing progress.
pub fn scan(db: &Db, target: &Target, progress: &mut (dyn FnMut(&str, usize, usize) + Send)) -> Scan {
    std::thread::scope(|s| {
        let camera = s.spawn(|| match camera::detect()? {
            Some(camera) => {
                let name = camera.name().to_owned();
                scan_camera(camera, db, &mut |done, total| progress(&name, done, total)).map(Some)
            }
            None => Ok(None),
        });
        let phone = match phone::detect(target) {
            Ok(Some(phone)) => scan_phone(phone, db).map(Some),
            other => other.map(|_| None),
        };
        Scan { camera: camera.join().expect("listing doesn't panic"), phone }
    })
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub camera: Option<CameraPlan>,
    pub phone: Option<PhonePlan>,
}

/// What a sync would copy. Records first that everything in the library counts as imported, and
/// everything the phone has from the library as sent, so a photo deleted later stays deleted
/// even if Ricochet didn't copy it.
pub fn plan(
    library: &Library,
    db: &Db,
    camera: Option<&CameraScan>,
    phone: Option<&PhoneScan>,
    restore: Restore,
) -> Result<Plan> {
    let have = library.scan()?;
    db.record_imported(library.root(), &have)?;
    let camera = match camera {
        Some(c) => Some(plan::camera(&c.files, &have, &db.imported(library.root())?, restore.camera)),
        None => None,
    };
    let phone = match phone {
        Some(p) => {
            let on_phone: Vec<&Photo> = have.iter().filter(|h| p.photos.get(&h.rel_path()) == Some(&h.size)).collect();
            db.record_sent(p.phone.id(), on_phone)?;
            let incoming: Vec<Photo> =
                camera.iter().flat_map(|c| &c.todo).filter_map(CameraFile::photo).collect();
            Some(plan::phone(&have, &incoming, &p.photos, &db.sent(p.phone.id())?, restore.phone))
        }
        None => None,
    };
    Ok(Plan { camera, phone })
}

/// How each side of a run ended: files copied, or why it stopped.
#[derive(Debug, Default)]
pub struct Outcome {
    pub camera: Option<Result<usize>>,
    pub phone: Option<Result<usize>>,
}

/// Runs `plan`: the camera's files into the library and on to the phone as each lands, and the
/// library's backlog to the phone meanwhile.
pub fn run(
    library: &Library,
    db: &Db,
    camera: Option<&mut CameraScan>,
    phone: Option<&PhoneScan>,
    plan: &Plan,
    cancel: &AtomicBool,
    on: &(dyn Fn(Side, Event) + Sync),
) -> Outcome {
    let (queue, jobs) = channel();
    let camera_job = camera.zip(plan.camera.as_ref());
    let phone_job = phone.zip(plan.phone.as_ref());
    // Photos the phone should get once the camera files them.
    let forward: HashSet<String> = match &phone_job {
        Some((_, p)) => p.incoming.iter().map(Photo::rel_path).collect(),
        None => HashSet::new(),
    };
    if let Some((_, p)) = &phone_job {
        for photo in &p.backlog {
            let _ = queue.send(photo.clone());
        }
    }
    std::thread::scope(|s| {
        let camera = camera_job.map(|(scan, plan)| {
            let queue = queue.clone();
            let forward = &forward;
            s.spawn(move || {
                import(scan.camera.as_mut(), library, db, &plan.todo, cancel, &|e| on(Side::Camera, e), &mut |photo| {
                    if forward.contains(&photo.rel_path()) {
                        let _ = queue.send(photo.clone());
                    }
                })
            })
        });
        // The queue closes once the camera is done with it.
        drop(queue);
        let phone = phone_job.map(|(scan, plan)| push(scan.phone.as_ref(), library, db, plan, jobs, cancel, on));
        Outcome {
            camera: camera.map(|t| t.join().expect("the import doesn't panic")),
            phone,
        }
    })
}

/// Downloads `todo` from the camera and files each into the library, handing each to `filed`.
fn import(
    camera: &mut dyn Camera,
    library: &Library,
    db: &Db,
    todo: &[CameraFile],
    cancel: &AtomicBool,
    on: &dyn Fn(Event),
    filed: &mut dyn FnMut(&Photo),
) -> Result<usize> {
    let incoming = library.incoming();
    fs::create_dir_all(&incoming)?;
    let mut done = 0;
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
        let photo = library.file(&staged, &file.name)?;
        db.record_imported(library.root(), [&photo])?;
        filed(&photo);
        done += got;
        on(Event::Bytes(done));
    }
    let _ = fs::remove_dir(&incoming);
    Ok(todo.len())
}

/// The phone side's shared progress: files started and bytes done across all uploaders.
#[derive(Default)]
struct Pushed {
    started: usize,
    bytes: u64,
    sent: usize,
    error: Option<Error>,
}

/// Sends what arrives on `jobs` over `UPLOADERS` connections at once, until the queue closes,
/// one upload fails, or `cancel`.
fn push(
    phone: &dyn Phone,
    library: &Library,
    db: &Db,
    plan: &PhonePlan,
    jobs: Receiver<Photo>,
    cancel: &AtomicBool,
    on: &(dyn Fn(Side, Event) + Sync),
) -> Result<usize> {
    if plan.files() == 0 {
        return Ok(0);
    }
    let uploaders = (0..UPLOADERS.min(plan.files())).map(|_| phone.uploader()).collect::<Result<Vec<_>>>()?;
    let phone = phone.id();
    let (jobs, pushed) = (Mutex::new(jobs), Mutex::new(Pushed::default()));
    let next = || {
        if cancel.load(Ordering::Relaxed) || pushed.lock().unwrap().error.is_some() {
            return None;
        }
        jobs.lock().unwrap().recv().ok()
    };
    let upload = |mut uploader: Box<dyn Uploader + Send>| {
        while let Some(photo) = next() {
            let index = {
                let mut p = pushed.lock().unwrap();
                p.started += 1;
                p.started - 1
            };
            on(Side::Phone, Event::File { index, name: photo.name.clone() });
            let mut reported = 0;
            let result = uploader.upload(&library.path(&photo), &photo, &mut |sent| {
                let bytes = {
                    let mut p = pushed.lock().unwrap();
                    p.bytes += sent - reported;
                    p.bytes
                };
                reported = sent;
                on(Side::Phone, Event::Bytes(bytes));
            });
            let mut p = pushed.lock().unwrap();
            match result.and_then(|()| db.record_sent(phone, [&photo])) {
                Ok(()) => p.sent += 1,
                Err(e) => {
                    p.error.get_or_insert(e);
                    return;
                }
            }
        }
    };
    std::thread::scope(|s| {
        for uploader in uploaders {
            s.spawn(|| upload(uploader));
        }
    });
    let pushed = pushed.into_inner().unwrap();
    match pushed.error {
        Some(e) => Err(e),
        // Work left on the queue means Stop came first.
        None if cancel.load(Ordering::Relaxed) && pushed.sent < plan.files() => Err(Error::Cancelled),
        None => Ok(pushed.sent),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::camera::Object;
    use crate::Kind;

    /// A camera whose files hold their own names as contents.
    struct FakeCamera(Vec<(u32, &'static str)>);

    impl Camera for FakeCamera {
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
            Ok(if parent == 0 { self.0.iter().map(|f| f.0).collect() } else { Vec::new() })
        }
        fn describe(&mut self, handle: u32) -> Result<Option<Object>> {
            let name = self.0.iter().find(|f| f.0 == handle).map(|f| f.1);
            let size = name.map(|n| n.len() as u64).unwrap_or(0);
            Ok(name.map(|n| Object { storage: 1, name: n.into(), size, created: String::new(), folder: false }))
        }
        fn download(&mut self, file: &CameraFile, to: &Path, _: &mut dyn FnMut(u64)) -> Result<()> {
            Ok(fs::write(to, &file.name)?)
        }
    }

    /// A phone holding uploads in memory; `fail` makes every upload fail.
    #[derive(Clone, Default)]
    struct FakePhone {
        files: Arc<Mutex<HashMap<String, u64>>>,
        fail: bool,
    }

    impl Phone for FakePhone {
        fn name(&self) -> &str {
            "Fake"
        }
        fn id(&self) -> &str {
            "udid"
        }
        fn list(&mut self, _: &HashMap<String, u64>) -> Result<Vec<Photo>> {
            let files = self.files.lock().unwrap();
            let photo = |(path, size): (&String, &u64)| {
                let name = path.rsplit('/').next().unwrap().to_owned();
                Photo { kind: Kind::of(&name).unwrap(), name, size: *size }
            };
            Ok(files.iter().map(photo).collect())
        }
        fn uploader(&self) -> Result<Box<dyn Uploader + Send>> {
            Ok(Box::new(self.clone()))
        }
    }

    impl Uploader for FakePhone {
        fn upload(&mut self, src: &Path, photo: &Photo, _: &mut dyn FnMut(u64)) -> Result<()> {
            if self.fail {
                return Err(Error::Phone("unplugged".into()));
            }
            self.files.lock().unwrap().insert(photo.rel_path(), fs::metadata(src)?.len());
            Ok(())
        }
    }

    struct World {
        _dir: tempfile::TempDir,
        library: Library,
        db: Db,
        phone: FakePhone,
    }

    impl World {
        fn new() -> World {
            let dir = tempfile::tempdir().unwrap();
            let library = Library::new(dir.path());
            World { library, db: Db::in_memory().unwrap(), phone: FakePhone::default(), _dir: dir }
        }

        fn scans(&self, camera: &[(u32, &'static str)]) -> (CameraScan, PhoneScan) {
            let camera = scan_camera(Box::new(FakeCamera(camera.to_vec())), &self.db, &mut |_, _| {}).unwrap();
            (camera, scan_phone(Box::new(self.phone.clone()), &self.db).unwrap())
        }

        fn sync(&self, camera: &[(u32, &'static str)], restore: Restore) -> (Plan, Outcome) {
            let (mut camera, phone) = self.scans(camera);
            let plan = plan(&self.library, &self.db, Some(&camera), Some(&phone), restore).unwrap();
            let outcome =
                run(&self.library, &self.db, Some(&mut camera), Some(&phone), &plan, &AtomicBool::new(false), &|_, _| {});
            (plan, outcome)
        }

        fn on_phone(&self) -> Vec<String> {
            let mut paths: Vec<String> = self.phone.files.lock().unwrap().keys().cloned().collect();
            paths.sort();
            paths
        }
    }

    #[test]
    fn the_camera_feeds_the_phone_in_one_run() {
        let w = World::new();
        fs::create_dir_all(w.library.root().join("RAW")).unwrap();
        fs::write(w.library.root().join("RAW/R0.DNG"), b"old").unwrap();

        let (plan, outcome) = w.sync(&[(10, "R1.DNG"), (11, "R1.JPG")], Restore::default());
        assert_eq!(plan.phone.as_ref().map(|p| (p.backlog.len(), p.incoming.len())), Some((1, 2)));
        assert_eq!(outcome.camera.unwrap().unwrap(), 2);
        assert_eq!(outcome.phone.unwrap().unwrap(), 3);
        assert_eq!(w.on_phone(), ["JPG/R1.JPG", "RAW/R0.DNG", "RAW/R1.DNG"]);
    }

    #[test]
    fn a_failing_phone_leaves_the_import_running() {
        let mut w = World::new();
        w.phone.fail = true;
        let (_, outcome) = w.sync(&[(10, "R1.DNG"), (11, "R2.DNG")], Restore::default());
        assert!(matches!(outcome.phone, Some(Err(Error::Phone(_)))));
        assert_eq!(outcome.camera.unwrap().unwrap(), 2);
        assert_eq!(w.library.scan().unwrap().len(), 2);
    }

    #[test]
    fn deleted_photos_stay_deleted_until_restored() {
        let w = World::new();
        let camera = [(10, "R1.DNG"), (11, "R2.DNG")];
        w.sync(&camera, Restore::default());
        fs::remove_file(w.library.root().join("RAW/R1.DNG")).unwrap();
        w.phone.files.lock().unwrap().remove("RAW/R2.DNG");

        let (plan, _) = w.sync(&camera, Restore::default());
        let deleted = |p: &Plan| {
            let camera = p.camera.as_ref().unwrap().deleted.iter().map(|f| f.name.clone());
            let phone = p.phone.as_ref().unwrap().deleted.iter().map(|f| f.name.clone());
            (camera.collect::<Vec<_>>(), phone.collect::<Vec<_>>())
        };
        assert_eq!(deleted(&plan), (vec!["R1.DNG".to_owned()], vec!["R2.DNG".to_owned()]));
        assert_eq!(w.library.scan().unwrap().len(), 1);

        let (plan, _) = w.sync(&camera, Restore { camera: true, phone: true });
        assert_eq!(deleted(&plan), (vec![], vec![]));
        assert_eq!(w.library.scan().unwrap().len(), 2);
        assert_eq!(w.on_phone(), ["RAW/R1.DNG", "RAW/R2.DNG"]);
    }

    #[test]
    fn a_phone_alone_gets_the_backlog() {
        let w = World::new();
        fs::create_dir_all(w.library.root().join("JPG")).unwrap();
        fs::write(w.library.root().join("JPG/R5.JPG"), b"jpeg").unwrap();
        let phone = scan_phone(Box::new(w.phone.clone()), &w.db).unwrap();
        let plan = plan(&w.library, &w.db, None, Some(&phone), Restore::default()).unwrap();
        let outcome = run(&w.library, &w.db, None, Some(&phone), &plan, &AtomicBool::new(false), &|_, _| {});
        assert_eq!((outcome.camera.is_none(), outcome.phone.unwrap().unwrap()), (true, 1));
    }
}
