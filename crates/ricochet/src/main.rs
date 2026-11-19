//! Ricochet's window: one Sync for camera → laptop → iPhone, with a lane per device.
//! `cargo run -p ricochet -- --snap out.png --scene importing` renders a state without devices.

mod view;
mod worker;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ricochet_core::sync::Restore;
use rune::{AppSpec, Tx, Waker};

pub use worker::{Leg, Side, Update};

/// One device's state, plus how its last run ended, which outlives a recheck.
#[derive(Default)]
pub struct Lane {
    pub leg: Leg,
    pub last: Option<String>,
}

#[derive(Default)]
pub struct State {
    pub camera: Lane,
    pub phone: Lane,
    pub library: Option<(usize, u64)>,
    /// Set while a worker runs; both sides stop between files when told to.
    worker: Option<Arc<AtomicBool>>,
    inbox: Arc<Mutex<Vec<Update>>>,
    waker: Option<Waker>,
}

impl State {
    pub fn lane(&self, side: Side) -> &Lane {
        match side {
            Side::Camera => &self.camera,
            Side::Phone => &self.phone,
        }
    }

    fn lane_mut(&mut self, side: Side) -> &mut Lane {
        match side {
            Side::Camera => &mut self.camera,
            Side::Phone => &mut self.phone,
        }
    }

    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }

    pub fn refresh(&mut self) {
        self.start(false, Restore::default());
    }

    pub fn sync(&mut self) {
        self.start(true, Restore::default());
    }

    /// Syncs, copying again what was deleted at `side`'s destination since it was copied.
    pub fn restore(&mut self, side: Side) {
        self.start(true, Restore { camera: side == Side::Camera, phone: side == Side::Phone });
    }

    /// Checks both devices, and with `run` syncs what the check found.
    fn start(&mut self, run: bool, restore: Restore) {
        let Some(waker) = self.waker.clone() else { return };
        if self.busy() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        for lane in [&mut self.camera, &mut self.phone] {
            lane.leg = Leg::Checking;
            if run {
                lane.last = None;
            }
        }
        self.worker = Some(cancel.clone());
        worker::spawn(run, restore, cancel, self.inbox.clone(), waker);
    }

    pub fn cancel(&mut self) {
        if let Some(c) = &self.worker {
            c.store(true, Ordering::Relaxed);
        }
    }
}

// The first check starts here: `poll` can run before `setup`, when there's no waker to hand a
// worker yet. Headless runs (screenshots, scenes) never touch devices.
fn setup(s: &mut State, waker: Waker) {
    s.waker = Some(waker);
    if !rune::is_headless() {
        s.refresh();
    }
}

fn poll(s: &mut State, _clock: f64, _tx: &mut Tx) -> bool {
    let updates = std::mem::take(&mut *s.inbox.lock().unwrap());
    let changed = !updates.is_empty();
    for update in updates {
        match update {
            Update::Leg(side, leg) => s.lane_mut(side).leg = leg,
            Update::Note(side, note) => s.lane_mut(side).last = Some(note),
            Update::Library(files, bytes) => s.library = Some((files, bytes)),
            Update::Finished => s.worker = None,
        }
    }
    changed
}

type Scene = (&'static str, fn(&mut State));

fn ready(device: &str, files: usize, bytes: u64, raw: usize, jpg: usize, deleted: usize) -> Leg {
    Leg::Ready { device: device.into(), files, bytes, raw, jpg, deleted }
}

const SCENES: &[Scene] = &[
    ("ready", |s| {
        s.library = Some((1184, 23_110_000_000));
        s.camera.leg = ready("RICOH GR IV", 48, 1_020_000_000, 24, 24, 0);
        s.phone.leg = ready("👻", 48, 1_020_000_000, 24, 24, 0);
    }),
    ("importing", |s| {
        s.library = Some((1184, 23_110_000_000));
        s.worker = Some(Arc::default());
        s.camera.leg = Leg::Running {
            device: "RICOH GR IV".into(), index: 17, files: 48, name: "R0000802.DNG".into(),
            done: 402_000_000, bytes: 1_020_000_000,
        };
        s.phone.leg = Leg::Running {
            device: "👻".into(), index: 9, files: 48, name: "R0000793.JPG".into(),
            done: 188_000_000, bytes: 1_020_000_000,
        };
    }),
    ("checking", |s| {
        s.library = Some((1184, 23_110_000_000));
        s.worker = Some(Arc::default());
        s.camera.leg = Leg::Listing { device: "RICOH GR IV".into(), done: 412, total: 1315 };
        s.phone.leg = Leg::Checking;
    }),
    ("absent", |s| {
        s.library = Some((0, 0));
        s.camera.leg = Leg::Absent;
        s.phone.leg = Leg::Absent;
    }),
    ("deleted", |s| {
        s.library = Some((1184, 23_110_000_000));
        s.camera.leg = ready("RICOH GR IV", 0, 0, 0, 0, 12);
        s.camera.last = Some("Imported 48 files".into());
        s.phone.leg = ready("👻", 0, 0, 0, 0, 3);
        s.phone.last = Some("Sent 48 files".into());
    }),
    ("failed", |s| {
        s.library = Some((1184, 23_110_000_000));
        s.camera.leg = ready("RICOH GR IV", 0, 0, 0, 0, 0);
        s.camera.last = Some("Imported 48 files".into());
        s.phone.leg = Leg::Failed { error: "phone: the phone is locked; unlock it and try again".into() };
    }),
];

fn main() {
    rune::run(AppSpec {
        fonts: rune_theme_opencode::FONTS,
        families: rune_theme_opencode::FAMILIES,
        scenes: SCENES,
        setup: Some(setup),
        poll: Some(poll),
        min_size: Some((480.0, 360.0)),
        ..AppSpec::new("Ricochet", (560.0, 420.0), State::default, view::view)
    });
}
