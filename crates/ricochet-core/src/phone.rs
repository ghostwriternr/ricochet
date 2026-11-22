//! The phone: one app's Documents folder, reached over USB through usbmuxd, which is the folder
//! that app shows under On My iPhone in Files. Nothing is mounted.

use std::collections::HashMap;
use std::path::Path;

use crate::{Error, Kind, Photo, Result};

/// Where photos go on the phone: `<app's Documents>/<folder>/<RAW|JPG|Video>/<name>`.
#[derive(Clone, Debug)]
pub struct Target {
    /// The bundle id of an app with file sharing on, such as LocalSend's.
    pub app: String,
    pub folder: String,
}

impl Default for Target {
    fn default() -> Target {
        Target { app: "org.localsend.localsendApp".into(), folder: "GR IV".into() }
    }
}

pub trait Phone {
    /// The name set on the phone ("Naresh's iPhone").
    fn name(&self) -> &str;
    /// Tells this phone and target folder apart from others: UDID, app and folder.
    fn id(&self) -> &str;
    /// Every photo already in the target folder. A path in `known` is taken to be that size
    /// without asking the phone, which saves a round trip per file.
    fn list(&mut self, known: &HashMap<String, u64>) -> Result<Vec<Photo>>;
    /// A new connection to upload over. Several can upload at once.
    fn uploader(&self) -> Result<Box<dyn Uploader + Send>>;
}

pub trait Uploader {
    /// Copies `src` to `photo`'s place, reporting bytes sent so far. A copy that stops halfway
    /// leaves only a `.part` file.
    fn upload(&mut self, src: &Path, photo: &Photo, progress: &mut dyn FnMut(u64)) -> Result<()>;
}

/// The first phone connected by USB and paired with this computer, or `None`.
pub fn detect(target: &Target) -> Result<Option<Box<dyn Phone + Send>>> {
    afc::detect(target)
}

mod afc {
    use super::*;
    use std::collections::HashSet;
    use std::io::Read;

    use idevice::afc::AfcClient;
    use idevice::afc::errors::AfcError;
    use idevice::afc::opcode::{AfcFopenMode, AfcOpcode};
    use idevice::afc::packet::{AfcPacket, AfcPacketHeader};
    use idevice::house_arrest::HouseArrestClient;
    use idevice::lockdown::LockdownClient;
    use idevice::provider::UsbmuxdProvider;
    use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection};
    use idevice::{IdeviceError, IdeviceService};
    use tokio::runtime::Runtime;

    /// AFC's largest write, as libimobiledevice and idevice use.
    const CHUNK: usize = 1 << 20;
    /// Writes sent before waiting for the first one's reply. idevice sends one and waits; with
    /// four out the link stays busy: 24 → 27.5 MB/s on one connection over USB 2.
    const IN_FLIGHT: usize = 4;

    fn err(e: IdeviceError) -> Error {
        match e {
            IdeviceError::Afc(AfcError::NoSpaceLeft) => {
                Error::Phone("the iPhone is out of space; free some up and sync again".into())
            }
            e => Error::Phone(e.to_string()),
        }
    }

    fn runtime() -> Result<Runtime> {
        Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
    }

    async fn connect(provider: &UsbmuxdProvider, target: &Target) -> Result<AfcClient> {
        let afc = HouseArrestClient::connect(provider).await.map_err(err)?;
        afc.vend_documents(target.app.clone()).await.map_err(err)
    }

    pub struct Afc {
        rt: Runtime,
        afc: AfcClient,
        provider: UsbmuxdProvider,
        target: Target,
        name: String,
        id: String,
        root: String,
    }

    pub fn detect(target: &Target) -> Result<Option<Box<dyn Phone + Send>>> {
        let rt = runtime()?;
        let found = rt.block_on(async {
            let Ok(mut mux) = UsbmuxdConnection::default().await else { return Ok(None) };
            let devices = mux.get_devices().await.map_err(err)?;
            let Some(device) = devices.into_iter().find(|d| d.connection_type == Connection::Usb) else {
                return Ok(None);
            };
            let provider = device.to_provider(UsbmuxdAddr::default(), "ricochet");
            let mut lockdown = LockdownClient::connect(&provider).await.map_err(err)?;
            let name = match lockdown.get_value(Some("DeviceName"), None).await.map_err(err)? {
                plist::Value::String(s) => s,
                _ => "iPhone".to_owned(),
            };
            let afc = connect(&provider, target).await?;
            let id = format!("{}:{}/{}", device.udid, target.app, target.folder);
            Ok::<_, Error>(Some((afc, provider, name, id)))
        })?;
        Ok(found.map(|(afc, provider, name, id)| {
            let root = format!("/Documents/{}", target.folder);
            Box::new(Afc { rt, afc, provider, target: target.clone(), name, id, root }) as Box<dyn Phone + Send>
        }))
    }

    async fn list_dir(afc: &mut AfcClient, path: &str) -> Result<Vec<String>> {
        match afc.list_dir(path).await {
            Ok(names) => Ok(names.into_iter().filter(|n| n != "." && n != "..").collect()),
            Err(IdeviceError::Afc(AfcError::ObjectNotFound)) => Ok(Vec::new()),
            Err(e) => Err(err(e)),
        }
    }

    impl Phone for Afc {
        fn name(&self) -> &str {
            &self.name
        }

        fn id(&self) -> &str {
            &self.id
        }

        fn list(&mut self, known: &HashMap<String, u64>) -> Result<Vec<Photo>> {
            let Afc { rt, afc, root, .. } = self;
            rt.block_on(async {
                let mut photos = Vec::new();
                for kind in Kind::ALL {
                    let dir = format!("{root}/{}", kind.folder());
                    for name in list_dir(afc, &dir).await? {
                        if Kind::of(&name) != Some(kind) {
                            continue;
                        }
                        let photo = Photo { kind, name, size: 0 };
                        if let Some(&size) = known.get(&photo.rel_path()) {
                            photos.push(Photo { size, ..photo });
                            continue;
                        }
                        let info = afc.get_file_info(format!("{dir}/{}", photo.name)).await.map_err(err)?;
                        if info.st_ifmt == "S_IFREG" {
                            photos.push(Photo { size: info.size as u64, ..photo });
                        }
                    }
                }
                photos.sort();
                Ok(photos)
            })
        }

        fn uploader(&self) -> Result<Box<dyn Uploader + Send>> {
            let rt = runtime()?;
            let afc = rt.block_on(connect(&self.provider, &self.target))?;
            // Packet numbers for pipelined writes come from a range idevice's own never reach.
            let conn = Conn { afc, root: self.root.clone(), made: HashSet::new(), packet: 1 << 40 };
            Ok(Box::new(AfcUploader { rt, conn }))
        }
    }

    struct AfcUploader {
        rt: Runtime,
        conn: Conn,
    }

    struct Conn {
        afc: AfcClient,
        root: String,
        /// Folders made over this connection, so each is made once.
        made: HashSet<String>,
        packet: u64,
    }

    impl Conn {
        /// Makes `dir` and its parents. Each fails harmlessly when the folder exists; a real
        /// failure shows at the write or rename that follows.
        async fn make_dirs(&mut self, dir: &str) {
            let mut path = String::new();
            for segment in dir.split('/').skip(1) {
                path = format!("{path}/{segment}");
                if self.made.insert(path.clone()) {
                    let _ = self.afc.mk_dir(path.clone()).await;
                }
            }
        }

        /// Writes all of `file` to the open `fd`, keeping `IN_FLIGHT` writes on the wire. idevice
        /// has no pipelined write, so this sends AFC packets itself; afcd answers each in order.
        /// Every reply is read, even after a failure, so the next request gets its own reply.
        async fn write(&mut self, fd: u64, file: &mut std::fs::File, progress: &mut dyn FnMut(u64)) -> Result<()> {
            let mut in_flight = 0;
            let wrote = self.send_writes(fd, file, progress, &mut in_flight).await;
            let mut replies = Ok(());
            for _ in 0..in_flight {
                if let Err(e) = self.afc.read().await {
                    replies = replies.and(Err(err(e)));
                }
            }
            wrote.and(replies)
        }

        async fn send_writes(
            &mut self,
            fd: u64,
            file: &mut std::fs::File,
            progress: &mut dyn FnMut(u64),
            in_flight: &mut usize,
        ) -> Result<()> {
            let mut sent = 0;
            let mut buf = vec![0; CHUNK];
            loop {
                let n = read_up_to(file, &mut buf)?;
                if n == 0 {
                    break;
                }
                let header_payload = fd.to_le_bytes().to_vec();
                let header_len = header_payload.len() as u64 + AfcPacketHeader::LEN;
                self.packet += 1;
                let header = AfcPacketHeader {
                    magic: idevice::afc::MAGIC,
                    entire_len: header_len + n as u64,
                    header_payload_len: header_len,
                    packet_num: self.packet,
                    operation: AfcOpcode::Write,
                };
                self.afc.send(AfcPacket { header, header_payload, payload: buf[..n].to_vec() }).await.map_err(err)?;
                *in_flight += 1;
                if *in_flight == IN_FLIGHT {
                    *in_flight -= 1;
                    self.afc.read().await.map_err(err)?;
                }
                sent += n as u64;
                progress(sent);
            }
            Ok(())
        }
    }

    /// Fills `buf` unless the file ends first, so every write but the last is a full chunk.
    fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut filled = 0;
        while filled < buf.len() {
            match file.read(&mut buf[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        Ok(filled)
    }

    impl Uploader for AfcUploader {
        fn upload(&mut self, src: &Path, photo: &Photo, progress: &mut dyn FnMut(u64)) -> Result<()> {
            let mut file = std::fs::File::open(src)?;
            self.rt.block_on(self.conn.upload(&mut file, photo, progress))
        }
    }

    impl Conn {
        async fn upload(&mut self, file: &mut std::fs::File, photo: &Photo, progress: &mut dyn FnMut(u64)) -> Result<()> {
            let dir = format!("{}/{}", self.root, photo.kind.folder());
            let dest = format!("{dir}/{}", photo.name);
            let part = format!("{dest}.part");
            self.make_dirs(&dir).await;
            let mut open = (AfcFopenMode::WrOnly as u64).to_le_bytes().to_vec();
            open.extend(part.as_bytes());
            let opened = self.afc.send_op(AfcOpcode::FileOpen, open, Vec::new()).await.map_err(err)?;
            let fd = opened
                .header_payload
                .get(..8)
                .and_then(|b| b.try_into().ok())
                .map(u64::from_le_bytes)
                .ok_or_else(|| Error::Phone("the phone didn't open the file".into()))?;
            let wrote = self.write(fd, file, progress).await;
            let closed = self.afc.send_op(AfcOpcode::FileClose, fd.to_le_bytes().to_vec(), Vec::new()).await;
            let placed = match wrote.and(closed.map(drop).map_err(err)) {
                Ok(()) => self.place(&part, dest, photo).await,
                Err(e) => Err(e),
            };
            // A partial copy would only take up space; the next sync sends the photo again.
            if placed.is_err() {
                let _ = self.afc.remove(part).await;
            }
            placed
        }

        /// Renames the finished `.part` into place once its size matches.
        async fn place(&mut self, part: &str, dest: String, photo: &Photo) -> Result<()> {
            let size = self.afc.get_file_info(part.to_owned()).await.map_err(err)?.size as u64;
            if size != photo.size {
                return Err(Error::Phone(format!("{}: sent {size} of {} bytes", photo.name, photo.size)));
            }
            self.afc.rename(part.to_owned(), dest).await.map_err(err)
        }
    }
}
