//! The app's steps without a window.
//!
//! ```text
//! ricochet-cli [status]       what each step would copy
//! ricochet-cli import | push  run a step
//! ```
//! `RICOCHET_LIBRARY` overrides the library folder.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use ricochet_core::transfer::{self, Event};
use ricochet_core::{Library, Result, camera, phone, plan};

fn gb(bytes: u64) -> String {
    format!("{:.2} GB", bytes as f64 / 1e9)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match args.as_slice() {
        [] => "status",
        [cmd] if ["status", "import", "push"].contains(&cmd.as_str()) => cmd,
        _ => {
            eprintln!("usage: ricochet-cli [status | import | push]");
            std::process::exit(2);
        }
    };
    let root = std::env::var_os("RICOCHET_LIBRARY").map(PathBuf::from).unwrap_or_else(Library::default_root);
    let library = Library::new(root);
    let have = library.scan()?;
    println!("library {}: {} files, {}", library.root().display(), have.len(), gb(have.iter().map(|p| p.size).sum()));
    let cancel = AtomicBool::new(false);
    let mut report = |e: Event| if let Event::File { index, name } = e { eprintln!("  {} {name}", index + 1) };

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
    Ok(())
}
