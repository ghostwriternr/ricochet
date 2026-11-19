//! Device work off the UI thread. Each check or sync is one short-lived thread that finds both
//! devices, posts snapshots of its progress to the inbox, and wakes the window.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ricochet_core::phone::Target;
use ricochet_core::plan::{CameraPlan, PhonePlan};
use ricochet_core::sync::{self, Event, Plan, Restore};
use ricochet_core::{Db, Error, Kind, Library};
use rune::Waker;

pub use ricochet_core::sync::Side;

#[derive(Clone, Debug, Default)]
pub enum Leg {
    #[default]
    Checking,
    /// Reading the camera's file list; `total` grows as folders are opened.
    Listing { device: String, done: usize, total: usize },
    /// The device isn't connected.
    Absent,
    /// `files` would be copied; zero means up to date. `deleted` were copied before and since
    /// deleted at the destination, so they're left out.
    Ready { device: String, files: usize, bytes: u64, raw: usize, jpg: usize, deleted: usize },
    Running { device: String, index: usize, files: usize, name: String, done: u64, bytes: u64 },
    Failed { error: String },
}

pub enum Update {
    Leg(Side, Leg),
    /// How a side's run ended, shown until the next run.
    Note(Side, String),
    Library(usize, u64),
    /// The worker is done.
    Finished,
}

pub fn library() -> Library {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    Library::new(ROOT.get_or_init(Library::default_root))
}

/// Opened once for the app's life; if it can't open, every check says why.
fn db() -> Result<&'static Db, String> {
    static DB: OnceLock<Result<Db, String>> = OnceLock::new();
    DB.get_or_init(|| Db::open(&Db::default_path()).map_err(|e| e.to_string())).as_ref().map_err(Clone::clone)
}

struct Post {
    inbox: Arc<Mutex<Vec<Update>>>,
    waker: Waker,
}

impl Post {
    fn send(&self, update: Update) {
        self.inbox.lock().unwrap().push(update);
        (self.waker)();
    }

    fn leg(&self, side: Side, leg: Leg) {
        self.send(Update::Leg(side, leg));
    }

    fn both(&self, leg: Leg) {
        self.leg(Side::Camera, leg.clone());
        self.leg(Side::Phone, leg);
    }
}

/// Checks both devices, and with `run` syncs what the check found.
pub fn spawn(run: bool, restore: Restore, cancel: Arc<AtomicBool>, inbox: Arc<Mutex<Vec<Update>>>, waker: Waker) {
    std::thread::spawn(move || {
        let post = Post { inbox, waker };
        if let Err(error) = work(&post, run, restore, &cancel) {
            post.both(Leg::Failed { error });
        }
        if let Ok(all) = library().scan() {
            post.send(Update::Library(all.len(), all.iter().map(|p| p.size).sum()));
        }
        post.send(Update::Finished);
    });
}

fn work(post: &Post, run: bool, restore: Restore, cancel: &AtomicBool) -> Result<(), String> {
    let (library, db, target) = (library(), db()?, Target::default());
    let mut last = Instant::now();
    let scan = sync::scan(db, &target, &mut |device, done, total| {
        if every_tenth(&mut last) {
            post.leg(Side::Camera, Leg::Listing { device: device.to_owned(), done, total });
        }
    });
    let mut camera = found(post, Side::Camera, scan.camera);
    let phone = found(post, Side::Phone, scan.phone);
    let plan = sync::plan(&library, db, camera.as_ref(), phone.as_ref(), restore).map_err(|e| e.to_string())?;
    let camera_name = camera.as_ref().map(|c| c.camera.name().to_owned()).unwrap_or_default();
    let phone_name = phone.as_ref().map(|p| p.phone.name().to_owned()).unwrap_or_default();
    ready(post, &plan, &camera_name, &phone_name);
    let files = plan.camera.as_ref().map_or(0, |c| c.todo.len()) + plan.phone.as_ref().map_or(0, PhonePlan::files);
    if !run || files == 0 {
        return Ok(());
    }

    let progress = Progress::new(post, &plan, &camera_name, &phone_name);
    let outcome = sync::run(&library, db, camera.as_mut(), phone.as_ref(), &plan, cancel, &|side, e| progress.on(side, e));

    // Show what's left: the camera's listing still holds, and the phone lists again cheaply.
    let phone = match phone {
        Some(p) => Some(sync::scan_phone(p.phone, db).map_err(|e| e.to_string())?),
        None => None,
    };
    let left = sync::plan(&library, db, camera.as_ref(), phone.as_ref(), Restore::default()).map_err(|e| e.to_string())?;
    let (camera_left, phone_left) = ready_legs(&left, &camera_name, &phone_name);
    for (side, result, left, verb) in
        [(Side::Camera, outcome.camera, camera_left, "Imported"), (Side::Phone, outcome.phone, phone_left, "Sent")]
    {
        let note = match result {
            None => continue,
            Some(Ok(0)) => None,
            Some(Ok(n)) => Some(format!("{verb} {n} {}", if n == 1 { "file" } else { "files" })),
            Some(Err(Error::Cancelled)) => Some("Stopped".into()),
            Some(Err(e)) => {
                post.leg(side, Leg::Failed { error: e.to_string() });
                continue;
            }
        };
        if let Some(note) = note {
            post.send(Update::Note(side, note));
        }
        if let Some(leg) = left {
            post.leg(side, leg);
        }
    }
    Ok(())
}

/// What a scan found on `side`, posting why when it found nothing.
fn found<T>(post: &Post, side: Side, result: ricochet_core::Result<Option<T>>) -> Option<T> {
    match result {
        Ok(None) => {
            post.leg(side, Leg::Absent);
            None
        }
        Ok(found) => found,
        Err(e) => {
            post.leg(side, Leg::Failed { error: e.to_string() });
            None
        }
    }
}

fn ready(post: &Post, plan: &Plan, camera: &str, phone: &str) {
    let (camera_leg, phone_leg) = ready_legs(plan, camera, phone);
    for (side, leg) in [(Side::Camera, camera_leg), (Side::Phone, phone_leg)] {
        if let Some(leg) = leg {
            post.leg(side, leg);
        }
    }
}

/// Each planned side as a `Ready` leg.
fn ready_legs(plan: &Plan, camera: &str, phone: &str) -> (Option<Leg>, Option<Leg>) {
    let camera = plan.camera.as_ref().map(|CameraPlan { todo, deleted }| {
        let (raw, jpg) = kinds(todo.iter().filter_map(|f| Kind::of(&f.name)));
        let bytes = todo.iter().map(|f| f.size).sum();
        Leg::Ready { device: camera.to_owned(), files: todo.len(), bytes, raw, jpg, deleted: deleted.len() }
    });
    let phone = plan.phone.as_ref().map(|p| {
        let (raw, jpg) = kinds(p.backlog.iter().chain(&p.incoming).map(|p| p.kind));
        Leg::Ready { device: phone.to_owned(), files: p.files(), bytes: p.bytes(), raw, jpg, deleted: p.deleted.len() }
    });
    (camera, phone)
}

/// (RAW count, JPG count).
fn kinds(kinds: impl Iterator<Item = Kind>) -> (usize, usize) {
    kinds.fold((0, 0), |(raw, jpg), k| match k {
        Kind::Raw => (raw + 1, jpg),
        Kind::Jpeg => (raw, jpg + 1),
        Kind::Video => (raw, jpg),
    })
}

/// True at most ten times a second, so progress doesn't flood the window.
fn every_tenth(last: &mut Instant) -> bool {
    let due = last.elapsed() >= Duration::from_millis(100);
    if due {
        *last = Instant::now();
    }
    due
}

/// Turns each side's events into `Running` snapshots, at most ten a second per side.
struct Progress<'a> {
    post: &'a Post,
    sides: [Mutex<Running>; 2],
}

struct Running {
    device: String,
    files: usize,
    bytes: u64,
    index: usize,
    name: String,
    done: u64,
    last: Instant,
}

impl<'a> Progress<'a> {
    fn new(post: &'a Post, plan: &Plan, camera: &str, phone: &str) -> Progress<'a> {
        let running = |device: &str, files, bytes| {
            let last = Instant::now() - Duration::from_secs(1);
            Mutex::new(Running { device: device.to_owned(), files, bytes, index: 0, name: String::new(), done: 0, last })
        };
        let (camera_files, camera_bytes) = match &plan.camera {
            Some(c) => (c.todo.len(), c.todo.iter().map(|f| f.size).sum()),
            None => (0, 0),
        };
        let (phone_files, phone_bytes) = plan.phone.as_ref().map_or((0, 0), |p| (p.files(), p.bytes()));
        Progress { post, sides: [running(camera, camera_files, camera_bytes), running(phone, phone_files, phone_bytes)] }
    }

    fn on(&self, side: Side, event: Event) {
        let mut r = self.sides[side as usize].lock().unwrap();
        let started = matches!(event, Event::File { .. });
        match event {
            Event::File { index, name } => (r.index, r.name) = (index, name),
            Event::Bytes(done) => r.done = done,
        }
        if started | every_tenth(&mut r.last) {
            let Running { device, index, files, name, done, bytes, .. } = &*r;
            let leg = Leg::Running {
                device: device.clone(),
                index: *index,
                files: *files,
                name: name.clone(),
                done: *done,
                bytes: *bytes,
            };
            self.post.leg(side, leg);
        }
    }
}
