//! What each step would copy, worked out from listings alone.

use std::collections::HashSet;

use crate::{Kind, Photo};

/// A file as the camera lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraFile {
    pub folder: String,
    pub name: String,
    pub size: u64,
}

/// Camera files the library doesn't have yet: no library file with the same name and size.
pub fn new_on_camera(camera: &[CameraFile], library: &[Photo]) -> Vec<CameraFile> {
    let have: HashSet<(&str, u64)> = library.iter().map(|p| (p.name.as_str(), p.size)).collect();
    camera
        .iter()
        .filter(|f| Kind::of(&f.name).is_some() && !have.contains(&(f.name.as_str(), f.size)))
        .cloned()
        .collect()
}

/// Library photos the phone doesn't have at the same path and size. A partial or stale copy on
/// the phone counts as missing and is replaced.
pub fn missing_on_phone(library: &[Photo], phone: &[Photo]) -> Vec<Photo> {
    let have: HashSet<&Photo> = phone.iter().collect();
    library.iter().filter(|p| !have.contains(p)).cloned().collect()
}

pub fn total_bytes<'a>(sizes: impl IntoIterator<Item = &'a u64>) -> u64 {
    sizes.into_iter().sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    
    fn photo(name: &str, size: u64) -> Photo {
        Photo { kind: Kind::of(name).unwrap(), name: name.into(), size }
    }

    fn cam(name: &str, size: u64) -> CameraFile {
        CameraFile { folder: "/DCIM/100RICOH".into(), name: name.into(), size }
    }

    #[test]
    fn imported_means_same_name_and_size() {
        let library = [photo("R1.DNG", 10), photo("R2.JPG", 5)];
        let camera = [cam("R1.DNG", 10), cam("R2.JPG", 6), cam("R3.DNG", 10), cam("INFO.TXT", 1)];
        let names: Vec<_> = new_on_camera(&camera, &library).into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["R2.JPG", "R3.DNG"]);
    }

    #[test]
    fn a_short_copy_on_the_phone_is_sent_again() {
        let library = [photo("R1.DNG", 10), photo("R2.DNG", 10)];
        let phone = [photo("R1.DNG", 10), photo("R2.DNG", 4)];
        assert_eq!(missing_on_phone(&library, &phone), [library[1].clone()]);
    }
}
