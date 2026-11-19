//! A sync without a window.
//!
//! ```text
//! ricochet-cli [status]                   what a sync would copy
//! ricochet-cli sync [--restore-deleted]   run one
//! ```
//! `RICOCHET_LIBRARY` overrides the library folder.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ricochet_core::phone::Target;
use ricochet_core::sync::{self, Event, Restore, Side};
use ricochet_core::{Db, Library, Result};

fn gb(bytes: u64) -> String {
    format!("{:.2} GB", bytes as f64 / 1e9)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (run, restore) = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["status"] => (false, false),
        ["sync"] => (true, false),
        ["sync", "--restore-deleted"] => (true, true),
        _ => {
            eprintln!("usage: ricochet-cli [status | sync [--restore-deleted]]");
            std::process::exit(2);
        }
    };
    let root = std::env::var_os("RICOCHET_LIBRARY").map(PathBuf::from).unwrap_or_else(Library::default_root);
    let library = Library::new(root);
    let db = Db::open(&Db::default_path())?;
    let have = library.scan()?;
    println!("library {}: {} files, {}", library.root().display(), have.len(), gb(have.iter().map(|p| p.size).sum()));

    let started = Instant::now();
    let scan = sync::scan(&db, &Target::default(), &mut |_, _, _| {});
    let (mut camera, phone) = (scan.camera?, scan.phone?);
    let restore = Restore { camera: restore, phone: restore };
    let plan = sync::plan(&library, &db, camera.as_ref(), phone.as_ref(), restore)?;
    match (&camera, &plan.camera) {
        (Some(c), Some(p)) => {
            let bytes = gb(p.todo.iter().map(|f| f.size).sum());
            println!("camera {}: {} new, {bytes}{}", c.camera.name(), p.todo.len(), deleted(p.deleted.len()));
        }
        _ => println!("camera: none"),
    }
    match (&phone, &plan.phone) {
        (Some(ph), Some(p)) => {
            let deleted = deleted(p.deleted.len());
            println!("phone {}: {} to send, {}{deleted}", ph.phone.name(), p.files(), gb(p.bytes()));
        }
        _ => println!("phone: none"),
    }
    println!("checked in {:.1} s", started.elapsed().as_secs_f64());
    if !run {
        return Ok(());
    }

    let report = |side: Side, e: Event| {
        if let Event::File { index, name } = e {
            eprintln!("  {} {} {name}", if side == Side::Camera { "import" } else { "send" }, index + 1);
        }
    };
    let started = Instant::now();
    let outcome = sync::run(&library, &db, camera.as_mut(), phone.as_ref(), &plan, &AtomicBool::new(false), &report);
    let secs = started.elapsed().as_secs_f64();
    let mut failed = None;
    for (what, result) in [("imported", outcome.camera), ("sent", outcome.phone)] {
        match result {
            Some(Ok(files)) => println!("{what} {files} files"),
            Some(Err(e)) => {
                eprintln!("{what}: {e}");
                failed = Some(e);
            }
            None => {}
        }
    }
    println!("synced in {secs:.1} s");
    failed.map_or(Ok(()), Err)
}

fn deleted(n: usize) -> String {
    match n {
        0 => String::new(),
        n => format!(" ({n} deleted since copying; --restore-deleted copies them again)"),
    }
}
