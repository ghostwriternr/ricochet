//! What Ricochet remembers between runs, in SQLite. The folders on the laptop and the phone say
//! what exists; this holds only what's slow or impossible to work out again: the camera's
//! descriptions of its files, what was ever imported, and what was ever sent to each phone.
//! Deleting it loses no photos. The next camera listing is slow once, and photos deleted since
//! they were copied are offered again once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, params};

use crate::camera::Object;
use crate::{Photo, Result};

/// Each entry upgrades the schema by one version, recorded in `user_version`.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE camera_objects (
        camera TEXT NOT NULL,
        handle INTEGER NOT NULL,
        storage INTEGER NOT NULL,
        name TEXT NOT NULL,
        size INTEGER NOT NULL,
        created TEXT NOT NULL,
        folder INTEGER NOT NULL,
        PRIMARY KEY (camera, handle)
    );
    CREATE TABLE imported (
        library TEXT NOT NULL,
        name TEXT NOT NULL,
        size INTEGER NOT NULL,
        PRIMARY KEY (library, name, size)
    );
    CREATE TABLE sent (
        phone TEXT NOT NULL,
        path TEXT NOT NULL,
        size INTEGER NOT NULL,
        PRIMARY KEY (phone, path)
    );",
];

/// Safe to share between threads; each call takes the connection in turn. Sizes are stored as
/// SQLite's signed integers.
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// `ricochet.db` in the user's data folder: `~/.local/share/ricochet` on Linux,
    /// `~/Library/Application Support/ricochet` on a Mac.
    pub fn default_path() -> PathBuf {
        let data = dirs::data_dir().or_else(|| dirs::home_dir().map(|home| home.join(".local/share")));
        data.unwrap_or_default().join("ricochet").join("ricochet.db")
    }

    pub fn open(path: &Path) -> Result<Db> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Db::migrate(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Db> {
        Db::migrate(Connection::open_in_memory()?)
    }

    fn migrate(mut conn: Connection) -> Result<Db> {
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (version, sql) in (1..).zip(MIGRATIONS).skip(version as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", version)?;
            tx.commit()?;
        }
        Ok(Db { conn: Mutex::new(conn) })
    }

    /// The last listing of `camera` (its serial number), by handle.
    pub fn camera_objects(&self, camera: &str) -> Result<HashMap<u32, Object>> {
        let conn = self.conn.lock().unwrap();
        let mut query =
            conn.prepare("SELECT handle, storage, name, size, created, folder FROM camera_objects WHERE camera = ?1")?;
        let rows = query.query_map([camera], |r| {
            let object = Object { storage: r.get(1)?, name: r.get(2)?, size: r.get::<_, i64>(3)? as u64, created: r.get(4)?, folder: r.get(5)? };
            Ok((r.get(0)?, object))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces the remembered listing of `camera`.
    pub fn set_camera_objects(&self, camera: &str, objects: &HashMap<u32, Object>) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM camera_objects WHERE camera = ?1", [camera])?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO camera_objects (camera, handle, storage, name, size, created, folder)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (handle, o) in objects {
                insert.execute(params![camera, handle, o.storage, o.name, o.size as i64, o.created, o.folder])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every (name, size) ever imported into the library at `root`, including photos since
    /// deleted from it.
    pub fn imported(&self, root: &Path) -> Result<HashSet<(String, u64)>> {
        let conn = self.conn.lock().unwrap();
        let mut query = conn.prepare("SELECT name, size FROM imported WHERE library = ?1")?;
        let rows = query.query_map([root.to_string_lossy()], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn record_imported<'a>(&self, root: &Path, photos: impl IntoIterator<Item = &'a Photo>) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare("INSERT OR IGNORE INTO imported (library, name, size) VALUES (?1, ?2, ?3)")?;
            let root = root.to_string_lossy();
            for p in photos {
                insert.execute(params![root, p.name, p.size as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// What was sent to `phone` (see [`Phone::id`](crate::phone::Phone::id)), by path under its
    /// target folder, with the size sent.
    pub fn sent(&self, phone: &str) -> Result<HashMap<String, u64>> {
        let conn = self.conn.lock().unwrap();
        let mut query = conn.prepare("SELECT path, size FROM sent WHERE phone = ?1")?;
        let rows = query.query_map([phone], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn record_sent<'a>(&self, phone: &str, photos: impl IntoIterator<Item = &'a Photo>) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare("INSERT OR REPLACE INTO sent (phone, path, size) VALUES (?1, ?2, ?3)")?;
            for p in photos {
                insert.execute(params![phone, p.rel_path(), p.size as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;

    #[test]
    fn reopening_keeps_what_was_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/ricochet.db");
        let photo = Photo { kind: Kind::Raw, name: "R1.DNG".into(), size: 10 };
        let object = Object { storage: 1, name: "R1.DNG".into(), size: 10, created: "20261119T101500".into(), folder: false };
        {
            let db = Db::open(&path).unwrap();
            db.record_imported(Path::new("/lib"), [&photo]).unwrap();
            db.record_sent("phone", [&photo]).unwrap();
            db.set_camera_objects("cam", &HashMap::from([(7, object.clone())])).unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.imported(Path::new("/lib")).unwrap(), HashSet::from([("R1.DNG".to_owned(), 10)]));
        assert!(db.imported(Path::new("/other")).unwrap().is_empty());
        assert_eq!(db.sent("phone").unwrap(), HashMap::from([("RAW/R1.DNG".to_owned(), 10)]));
        assert!(db.sent("other").unwrap().is_empty());
        assert_eq!(db.camera_objects("cam").unwrap(), HashMap::from([(7, object)]));
    }
}
