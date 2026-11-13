//! Device work off the UI thread. Each scan or run is one short-lived thread that opens its
//! device, posts snapshots of its progress to the inbox, and wakes the window.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ricochet_core::phone::{self, Target};
use ricochet_core::transfer::{self, Event};
use ricochet_core::{Error, Kind, Library, Result, camera, plan};
use rune::Waker;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Import,
    Push,
}

#[derive(Clone, Debug, Default)]
pub enum Leg {
    #[default]
    Checking,
    /// No camera (for import) or no phone (for push) is connected.
    Absent,
    /// `files` would be copied; zero means up to date.
    Ready { device: String, files: usize, bytes: u64, raw: usize, jpg: usize },
    Running { device: String, index: usize, files: usize, name: String, done: u64, bytes: u64 },
    Failed { error: String },
}

pub enum Update {
    Leg(Side, Leg),
    /// How the last run ended, shown until the next run.
    Last(Side, String),
    Library(usize, u64),
}

pub fn library() -> Library {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    Library::new(home.join("Pictures").join("GR IV"))
}

struct Post {
    side: Side,
    inbox: Arc<Mutex<Vec<Update>>>,
    waker: Waker,
}

impl Post {
    fn send(&self, update: Update) {
        self.inbox.lock().unwrap().push(update);
        (self.waker)();
    }

    fn leg(&self, leg: Leg) {
        self.send(Update::Leg(self.side, leg));
    }
}

pub fn spawn(side: Side, run: bool, cancel: Arc<AtomicBool>, inbox: Arc<Mutex<Vec<Update>>>, waker: Waker) {
    std::thread::spawn(move || {
        let post = Post { side, inbox, waker };
        let library = library();
        let result = match side {
            Side::Import => import(&post, &library, run, &cancel),
            Side::Push => push(&post, &library, run, &cancel),
        };
        match result {
            Ok(Some(note)) => post.send(Update::Last(side, note)),
            Ok(None) => {}
            Err(Error::Cancelled) => post.send(Update::Last(side, "Stopped".into())),
            Err(e) => post.leg(Leg::Failed { error: e.to_string() }),
        }
        if let Ok(all) = library.scan() {
            post.send(Update::Library(all.len(), all.iter().map(|p| p.size).sum()));
        }
    });
}

/// (files, bytes, RAW count, JPG count) of a plan.
type Counts = (usize, u64, usize, usize);

fn counts<'a>(kinds: impl Iterator<Item = Option<Kind>>, sizes: impl Iterator<Item = &'a u64>) -> Counts {
    let (mut raw, mut jpg, mut files) = (0, 0, 0);
    for k in kinds {
        files += 1;
        match k {
            Some(Kind::Raw) => raw += 1,
            Some(Kind::Jpeg) => jpg += 1,
            _ => {}
        }
    }
    (files, sizes.sum(), raw, jpg)
}

/// Turns transfer events into `Running` snapshots, at most ten a second.
fn progress<'a>(post: &'a Post, device: &'a str, names: Vec<String>, bytes: u64) -> impl FnMut(Event) + 'a {
    let files = names.len();
    let (mut index, mut done, mut sent) = (0, 0, Instant::now() - Duration::from_secs(1));
    move |event| {
        match event {
            Event::File { index: i, .. } => index = i,
            Event::Bytes(b) => done = b,
        }
        if matches!(event, Event::File { .. }) || sent.elapsed() >= Duration::from_millis(100) {
            sent = Instant::now();
            let name = names[index].clone();
            post.leg(Leg::Running { device: device.to_owned(), index, files, name, done, bytes });
        }
    }
}

fn import(post: &Post, library: &Library, run: bool, cancel: &AtomicBool) -> Result<Option<String>> {
    let Some(mut cam) = camera::detect()? else {
        post.leg(Leg::Absent);
        return Ok(None);
    };
    let device = cam.name().to_owned();
    let todo = plan::new_on_camera(&cam.list()?, &library.scan()?);
    let (files, bytes, raw, jpg) = counts(todo.iter().map(|f| Kind::of(&f.name)), todo.iter().map(|f| &f.size));
    if !run || files == 0 {
        post.leg(Leg::Ready { device, files, bytes, raw, jpg });
        return Ok(None);
    }
    let names = todo.iter().map(|f| f.name.clone()).collect();
    transfer::import(cam.as_mut(), library, &todo, cancel, &mut progress(post, &device, names, bytes))?;
    post.leg(Leg::Ready { device, files: 0, bytes: 0, raw: 0, jpg: 0 });
    Ok(Some(format!("Imported {files} {}", if files == 1 { "file" } else { "files" })))
}

fn push(post: &Post, library: &Library, run: bool, cancel: &AtomicBool) -> Result<Option<String>> {
    let Some(mut ph) = phone::detect(&Target::default())? else {
        post.leg(Leg::Absent);
        return Ok(None);
    };
    let device = ph.name().to_owned();
    let todo = plan::missing_on_phone(&library.scan()?, &ph.list()?);
    let (files, bytes, raw, jpg) = counts(todo.iter().map(|p| Some(p.kind)), todo.iter().map(|p| &p.size));
    if !run || files == 0 {
        post.leg(Leg::Ready { device, files, bytes, raw, jpg });
        return Ok(None);
    }
    let names = todo.iter().map(|p| p.name.clone()).collect();
    transfer::push(ph.as_mut(), library, &todo, cancel, &mut progress(post, &device, names, bytes))?;
    post.leg(Leg::Ready { device, files: 0, bytes: 0, raw: 0, jpg: 0 });
    Ok(Some(format!("Sent {files} {}", if files == 1 { "file" } else { "files" })))
}
