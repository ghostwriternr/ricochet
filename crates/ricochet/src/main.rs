//! Ricochet's window: camera → laptop, then laptop → iPhone, one button each.
//! `cargo run -p ricochet -- --snap out.png --scene importing` renders a state without devices.

mod view;
mod worker;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rune::{AppSpec, Tx, Waker};

pub use worker::{Leg, Side, Update};

/// One step's state, plus the outcome of its last run, which outlives a rescan.
#[derive(Default)]
pub struct Step {
    pub leg: Leg,
    pub last: Option<String>,
    cancel: Option<Arc<AtomicBool>>,
}

impl Step {
    pub fn busy(&self) -> bool {
        matches!(self.leg, Leg::Checking | Leg::Running { .. })
    }
}

#[derive(Default)]
pub struct State {
    pub import: Step,
    pub push: Step,
    pub library: Option<(usize, u64)>,
    inbox: Arc<Mutex<Vec<Update>>>,
    waker: Option<Waker>,
    started: bool,
}

impl State {
    pub fn step_mut(&mut self, side: Side) -> &mut Step {
        match side {
            Side::Import => &mut self.import,
            Side::Push => &mut self.push,
        }
    }

    pub fn refresh(&mut self) {
        for side in [Side::Import, Side::Push] {
            if !self.step_mut(side).busy() {
                self.start(side, false);
            }
        }
    }

    /// Scans `side`, and with `run` copies what the scan found.
    pub fn start(&mut self, side: Side, run: bool) {
        let Some(waker) = self.waker.clone() else { return };
        let cancel = Arc::new(AtomicBool::new(false));
        let step = self.step_mut(side);
        step.leg = Leg::Checking;
        if run {
            step.last = None;
        }
        step.cancel = Some(cancel.clone());
        worker::spawn(side, run, cancel, self.inbox.clone(), waker);
    }

    pub fn cancel(&mut self, side: Side) {
        if let Some(c) = &self.step_mut(side).cancel {
            c.store(true, Ordering::Relaxed);
        }
    }
}

fn setup(s: &mut State, waker: Waker) {
    s.waker = Some(waker);
}

fn poll(s: &mut State, _clock: f64, _tx: &mut Tx) -> bool {
    if !s.started && !rune::is_headless() {
        s.started = true;
        s.refresh();
        return true;
    }
    let updates = std::mem::take(&mut *s.inbox.lock().unwrap());
    let changed = !updates.is_empty();
    for update in updates {
        match update {
            Update::Leg(side, leg) => s.step_mut(side).leg = leg,
            Update::Last(side, note) => {
                let step = s.step_mut(side);
                step.last = Some(note);
                step.cancel = None;
            }
            Update::Library(files, bytes) => s.library = Some((files, bytes)),
        }
    }
    changed
}

type Scene = (&'static str, fn(&mut State));

const SCENES: &[Scene] = &[
    ("ready", |s| {
        s.library = Some((1184, 23_110_000_000));
        s.import.leg = Leg::Ready { device: "RICOH GR IV".into(), files: 48, bytes: 1_020_000_000, raw: 24, jpg: 24 };
        s.push.leg = Leg::Ready { device: "👻".into(), files: 0, bytes: 0, raw: 0, jpg: 0 };
    }),
    ("importing", |s| {
        s.library = Some((1184, 23_110_000_000));
        s.import.leg = Leg::Running {
            device: "RICOH GR IV".into(), index: 17, files: 48, name: "R0000802.DNG".into(),
            done: 402_000_000, bytes: 1_020_000_000,
        };
        s.push.leg = Leg::Ready { device: "👻".into(), files: 0, bytes: 0, raw: 0, jpg: 0 };
    }),
    ("absent", |s| {
        s.library = Some((0, 0));
        s.import.leg = Leg::Absent;
        s.push.leg = Leg::Absent;
    }),
    ("failed", |s| {
        s.library = Some((1184, 23_110_000_000));
        s.import.leg = Leg::Ready { device: "RICOH GR IV".into(), files: 0, bytes: 0, raw: 0, jpg: 0 };
        s.import.last = Some("Imported 48 photos".into());
        s.push.leg = Leg::Failed { error: "phone: the phone is locked; unlock it and try again".into() };
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
