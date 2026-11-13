//! The laptop's copy: `<root>/<RAW|JPG|Video>/<name>`, flat, the same layout the phone gets.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Raw,
    Jpeg,
    Video,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Raw, Kind::Jpeg, Kind::Video];

    /// What a camera file is, by extension; `None` for anything Ricochet doesn't copy.
    pub fn of(name: &str) -> Option<Kind> {
        let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "dng" => Some(Kind::Raw),
            "jpg" | "jpeg" => Some(Kind::Jpeg),
            "mov" | "mp4" => Some(Kind::Video),
            _ => None,
        }
    }

    pub fn folder(self) -> &'static str {
        match self {
            Kind::Raw => "RAW",
            Kind::Jpeg => "JPG",
            Kind::Video => "Video",
        }
    }
}

/// One photo (or clip) where it's filed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Photo {
    pub kind: Kind,
    pub name: String,
    pub size: u64,
}

impl Photo {
    /// `RAW/R0000185.DNG`, relative to a library or phone root.
    pub fn rel_path(&self) -> String {
        format!("{}/{}", self.kind.folder(), self.name)
    }
}

pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn new(root: impl Into<PathBuf>) -> Library {
        Library { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, photo: &Photo) -> PathBuf {
        self.root.join(photo.kind.folder()).join(&photo.name)
    }

    /// Where downloads land before they're filed. Same filesystem as the library, so filing is
    /// a rename.
    pub fn incoming(&self) -> PathBuf {
        self.root.join(".incoming")
    }

    /// Every filed photo. Anything outside the layout (other folders, stray files) is ignored.
    pub fn scan(&self) -> Result<Vec<Photo>> {
        let mut photos = Vec::new();
        for kind in Kind::ALL {
            let files = match fs::read_dir(self.root.join(kind.folder())) {
                Ok(files) => files,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for file in files {
                let file = file?;
                let meta = file.metadata()?;
                let Some(name) = file.file_name().to_str().map(str::to_owned) else { continue };
                if meta.is_file() && Kind::of(&name) == Some(kind) {
                    photos.push(Photo { kind, name, size: meta.len() });
                }
            }
        }
        photos.sort();
        Ok(photos)
    }

    /// Files a finished download under its kind. An identical file already there
    /// (same size) is a no-op; a different one is a conflict and nothing is overwritten.
    pub fn file(&self, staged: &Path, name: &str) -> Result<Photo> {
        let kind = Kind::of(name).ok_or_else(|| Error::Camera(format!("{name}: not a photo")))?;
        let size = fs::metadata(staged)?.len();
        let photo = Photo { kind, name: name.to_owned(), size };
        let dest = self.path(&photo);
        match fs::metadata(&dest) {
            Ok(m) if m.len() == size => {
                fs::remove_file(staged)?;
                return Ok(photo);
            }
            Ok(_) => return Err(Error::Conflict(dest)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        fs::create_dir_all(dest.parent().expect("a filed photo has a folder"))?;
        // `hard_link` refuses to replace, unlike `rename`, so a racing copy can't be clobbered.
        fs::hard_link(staged, &dest)?;
        fs::remove_file(staged)?;
        Ok(photo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ricochet-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn filing_sorts_by_kind_and_never_overwrites() {
        let lib = Library::new(temp_root("file"));
        let stage = |name: &str, bytes: &[u8]| {
            fs::create_dir_all(lib.incoming()).unwrap();
            let p = lib.incoming().join(name);
            fs::write(&p, bytes).unwrap();
            p
        };

        let raw = lib.file(&stage("R1.DNG", b"raw"), "R1.DNG").unwrap();
        let jpg = lib.file(&stage("R1.JPG", b"jpeg"), "R1.JPG").unwrap();
        assert_eq!(raw.rel_path(), "RAW/R1.DNG");
        assert_eq!(lib.scan().unwrap(), vec![raw.clone(), jpg]);

        // The same file again is fine; a different one under the same name is refused.
        lib.file(&stage("R1.DNG", b"raw"), "R1.DNG").unwrap();
        assert!(matches!(lib.file(&stage("R1.DNG", b"other"), "R1.DNG"), Err(Error::Conflict(_))));
        assert_eq!(fs::read(lib.path(&raw)).unwrap(), b"raw");
    }

    #[test]
    fn scan_skips_what_isnt_in_the_layout() {
        let root = temp_root("scan");
        fs::create_dir_all(root.join("100RICOH")).unwrap();
        fs::write(root.join("100RICOH/R2.DNG"), b"x").unwrap();
        fs::create_dir_all(root.join("RAW")).unwrap();
        fs::write(root.join("RAW/notes.txt"), b"x").unwrap();
        fs::write(root.join("RAW/R3.JPG"), b"x").unwrap();
        assert!(Library::new(root).scan().unwrap().is_empty());
    }
}
