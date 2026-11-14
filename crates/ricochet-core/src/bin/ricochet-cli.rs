//! The app's steps without a window, plus a one-off move into the library.
//!
//! ```text
//! ricochet-cli [status]           what each step would copy
//! ricochet-cli import | push      run a step
//! ricochet-cli adopt DIR [--go]   move a folder of camera files into the library by kind
//! ```
//! `adopt` only prints its plan unless given `--go`. It moves, never copies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ricochet_core::transfer::{self, Event};
use ricochet_core::{Error, Kind, Library, Result, camera, phone, plan};

fn gb(bytes: u64) -> String {
    format!("{:.2} GB", bytes as f64 / 1e9)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let go = args.iter().any(|a| a == "--go");
    let root = std::env::var_os("RICOCHET_LIBRARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join("Pictures/GR IV"));
    let library = Library::new(root);
    let have = library.scan()?;
    println!("library {}: {} files, {}", library.root().display(), have.len(), gb(have.iter().map(|p| p.size).sum()));
    let cancel = AtomicBool::new(false);
    let mut report = |e: Event| if let Event::File { index, name } = e { eprintln!("  {} {name}", index + 1) };

    match args.first().map(String::as_str).unwrap_or("status") {
        cmd @ ("status" | "import" | "push") => {
            if cmd != "push" {
                match camera::detect()? {
                    None => println!("camera: none"),
                    Some(mut cam) => {
                        let todo = plan::new_on_camera(&cam.list(&mut |_, _| {})?, &have);
                        println!("camera {}: {} new, {}", cam.name(), todo.len(), gb(todo.iter().map(|f| f.size).sum()));
                        if cmd == "import" {
                            transfer::import(cam.as_mut(), &library, &todo, &cancel, &mut report)?;
                        }
                    }
                }
            }
            if cmd != "import" {
                let have = library.scan()?;
                match phone::detect(&phone::Target::default())? {
                    None => println!("phone: none"),
                    Some(mut ph) => {
                        let todo = plan::missing_on_phone(&have, &ph.list()?);
                        println!("phone {}: {} to send, {}", ph.name(), todo.len(), gb(todo.iter().map(|p| p.size).sum()));
                        if cmd == "push" {
                            transfer::push(ph.as_mut(), &library, &todo, &cancel, &mut report)?;
                        }
                    }
                }
            }
        }
        "adopt" => {
            let dir = args.get(1).filter(|a| *a != "--go").ok_or_else(|| Error::Camera("usage: adopt DIR [--go]".into()))?;
            adopt(&library, Path::new(dir), go)?;
        }
        other => eprintln!("unknown command {other}"),
    }
    Ok(())
}

fn adopt(library: &Library, dir: &Path, go: bool) -> Result<()> {
    let mut by_kind: BTreeMap<Kind, (usize, u64)> = BTreeMap::new();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(kind) = Kind::of(&name).filter(|_| entry.path().is_file()) else {
            println!("  skip {name}");
            continue;
        };
        let e = by_kind.entry(kind).or_default();
        e.0 += 1;
        e.1 += entry.metadata()?.len();
        files.push((entry.path(), name));
    }
    for (kind, (n, bytes)) in &by_kind {
        println!("  {}: {n} files, {}", kind.folder(), gb(*bytes));
    }
    if !go {
        println!("{} files would move into {}; run again with --go", files.len(), library.root().display());
        return Ok(());
    }
    for (path, name) in &files {
        library.file(path, name)?;
    }
    println!("moved {} files", files.len());
    Ok(())
}
