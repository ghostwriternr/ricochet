//! What a sync would copy, worked out from listings and what Ricochet remembers.

use std::collections::{HashMap, HashSet};

use crate::{Kind, Photo};

/// A file as the camera lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraFile {
    /// The camera's handle for it.
    pub id: u32,
    pub name: String,
    /// 0 when the camera can't say, as for files over 4 GB.
    pub size: u64,
}

impl CameraFile {
    /// The photo this becomes in the library, if it's a kind Ricochet copies.
    pub fn photo(&self) -> Option<Photo> {
        Some(Photo { kind: Kind::of(&self.name)?, name: self.name.clone(), size: self.size })
    }
}

/// Camera files to import, and those left out because they were imported before and since
/// deleted from the library.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CameraPlan {
    pub todo: Vec<CameraFile>,
    pub deleted: Vec<CameraFile>,
}

/// A camera file is in the library when a library file has the same name and size, or just the
/// same name when the camera doesn't know the size. `imported` is every (name, size) ever
/// imported; one of those the library lacks was deleted there, and is planned only if `restore`.
pub fn camera(camera: &[CameraFile], library: &[Photo], imported: &HashSet<(String, u64)>, restore: bool) -> CameraPlan {
    let have: HashSet<(&str, u64)> = library.iter().map(|p| (p.name.as_str(), p.size)).collect();
    let named: HashSet<&str> = library.iter().map(|p| p.name.as_str()).collect();
    let was_imported: HashSet<(&str, u64)> = imported.iter().map(|(n, s)| (n.as_str(), *s)).collect();
    let was_named: HashSet<&str> = imported.iter().map(|(n, _)| n.as_str()).collect();
    let mut plan = CameraPlan::default();
    for f in camera.iter().filter(|f| Kind::of(&f.name).is_some()) {
        let (in_library, before) = match f.size {
            0 => (named.contains(f.name.as_str()), was_named.contains(f.name.as_str())),
            size => (have.contains(&(f.name.as_str(), size)), was_imported.contains(&(f.name.as_str(), size))),
        };
        match (in_library, before && !restore) {
            (true, _) => {}
            (false, true) => plan.deleted.push(f.clone()),
            (false, false) => plan.todo.push(f.clone()),
        }
    }
    plan
}

/// What the phone should get: photos already in the library (`backlog`) and photos the camera is
/// about to import (`incoming`), with the ones left out because they were sent before and since
/// deleted from the phone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhonePlan {
    pub backlog: Vec<Photo>,
    pub incoming: Vec<Photo>,
    pub deleted: Vec<Photo>,
}

impl PhonePlan {
    pub fn files(&self) -> usize {
        self.backlog.len() + self.incoming.len()
    }

    pub fn bytes(&self) -> u64 {
        self.backlog.iter().chain(&self.incoming).map(|p| p.size).sum()
    }
}

enum Need {
    Send,
    /// The phone has it at the same path and size.
    Has,
    /// Sent before at this size, and since deleted from the phone.
    Deleted,
}

/// Whether the phone needs `photo`. A short or stale copy counts as missing and is replaced.
/// `sent` is what went to this phone before, by path and size.
fn phone_needs(photo: &Photo, phone: &HashMap<String, u64>, sent: &HashMap<String, u64>) -> Need {
    let path = photo.rel_path();
    match phone.get(&path) {
        Some(&size) if size == photo.size => Need::Has,
        Some(_) => Need::Send,
        None if sent.get(&path) == Some(&photo.size) => Need::Deleted,
        None => Need::Send,
    }
}

/// `phone` maps paths on the phone to sizes; `incoming` is what the camera will import.
pub fn phone(
    library: &[Photo],
    incoming: &[Photo],
    phone: &HashMap<String, u64>,
    sent: &HashMap<String, u64>,
    restore: bool,
) -> PhonePlan {
    let mut plan = PhonePlan::default();
    for (photo, from_camera) in library.iter().map(|p| (p, false)).chain(incoming.iter().map(|p| (p, true))) {
        let list = match phone_needs(photo, phone, sent) {
            Need::Has => continue,
            Need::Deleted if !restore => &mut plan.deleted,
            _ if from_camera => &mut plan.incoming,
            _ => &mut plan.backlog,
        };
        list.push(photo.clone());
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(name: &str, size: u64) -> Photo {
        Photo { kind: Kind::of(name).unwrap(), name: name.into(), size }
    }

    fn cam(name: &str, size: u64) -> CameraFile {
        CameraFile { id: 1, name: name.into(), size }
    }

    fn names(files: &[CameraFile]) -> Vec<&str> {
        files.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn imported_means_same_name_and_size() {
        let library = [photo("R1.DNG", 10), photo("R2.JPG", 5)];
        let camera = [cam("R1.DNG", 10), cam("R2.JPG", 6), cam("R3.DNG", 10), cam("INFO.TXT", 1)];
        let plan = super::camera(&camera, &library, &HashSet::new(), false);
        assert_eq!(names(&plan.todo), ["R2.JPG", "R3.DNG"]);
    }

    #[test]
    fn a_clip_too_long_to_size_matches_by_name() {
        let library = [photo("R1.MOV", 5_000_000_000)];
        let camera = [cam("R1.MOV", 0), cam("R2.MOV", 0)];
        assert_eq!(names(&super::camera(&camera, &library, &HashSet::new(), false).todo), ["R2.MOV"]);
    }

    #[test]
    fn a_photo_deleted_from_the_library_stays_out_unless_restored() {
        let imported = HashSet::from([("R1.DNG".to_owned(), 10), ("R2.MOV".to_owned(), 5_000_000_000)]);
        let camera = [cam("R1.DNG", 10), cam("R2.MOV", 0), cam("R3.DNG", 10)];
        let plan = super::camera(&camera, &[], &imported, false);
        assert_eq!((names(&plan.todo), names(&plan.deleted)), (vec!["R3.DNG"], vec!["R1.DNG", "R2.MOV"]));
        let plan = super::camera(&camera, &[], &imported, true);
        assert_eq!((plan.todo.len(), plan.deleted.len()), (3, 0));
    }

    #[test]
    fn the_phone_gets_short_copies_again_but_not_deleted_ones() {
        let library = [photo("R1.DNG", 10), photo("R2.DNG", 10), photo("R3.DNG", 10)];
        let incoming = [photo("R4.DNG", 10)];
        let on_phone = HashMap::from([("RAW/R1.DNG".to_owned(), 10), ("RAW/R2.DNG".to_owned(), 4)]);
        let sent = HashMap::from([("RAW/R1.DNG".to_owned(), 10), ("RAW/R3.DNG".to_owned(), 10)]);
        let plan = phone(&library, &incoming, &on_phone, &sent, false);
        assert_eq!(plan.backlog, [library[1].clone()]);
        assert_eq!(plan.incoming, incoming);
        assert_eq!(plan.deleted, [library[2].clone()]);
        assert_eq!(phone(&library, &incoming, &on_phone, &sent, true).backlog.len(), 2);
    }
}
