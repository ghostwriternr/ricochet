//! The phone: one app's Documents folder, reached over USB through usbmuxd, which is the folder
//! that app shows under On My iPhone in Files. Nothing is mounted.

use std::io::Read;
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
    /// Every photo already in the target folder.
    fn list(&mut self) -> Result<Vec<Photo>>;
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
    use idevice::afc::AfcClient;
    use idevice::afc::opcode::AfcFopenMode;
    use idevice::house_arrest::HouseArrestClient;
    use idevice::lockdown::LockdownClient;
    use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection};
    use idevice::{IdeviceError, IdeviceService};
    use tokio::runtime::Runtime;

    const CHUNK: usize = 1 << 20;

    fn err(e: IdeviceError) -> Error {
        Error::Phone(e.to_string())
    }

    pub struct Afc {
        rt: Runtime,
        afc: AfcClient,
        name: String,
        root: String,
    }

    pub fn detect(target: &Target) -> Result<Option<Box<dyn Phone + Send>>> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
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
            let afc = HouseArrestClient::connect(&provider).await.map_err(err)?;
            let afc = afc.vend_documents(target.app.clone()).await.map_err(err)?;
            Ok::<_, Error>(Some((afc, name)))
        })?;
        Ok(found.map(|(afc, name)| {
            let root = format!("/Documents/{}", target.folder);
            Box::new(Afc { rt, afc, name, root }) as Box<dyn Phone + Send>
        }))
    }

    impl Afc {
        /// Makes `photo`'s folder and its parents. Each fails harmlessly when the folder exists;
        /// a real failure shows at the write or rename that follows.
        async fn make_dirs(afc: &mut AfcClient, root: &str, photo: &Photo) {
            let mut dir = String::new();
            for segment in format!("{root}/{}", photo.kind.folder()).split('/').skip(1) {
                dir = format!("{dir}/{segment}");
                let _ = afc.mk_dir(dir.clone()).await;
            }
        }

        async fn list_dir(afc: &mut AfcClient, path: &str) -> Result<Vec<String>> {
            match afc.list_dir(path).await {
                Ok(names) => Ok(names.into_iter().filter(|n| n != "." && n != "..").collect()),
                Err(IdeviceError::Afc(idevice::afc::errors::AfcError::ObjectNotFound)) => Ok(Vec::new()),
                Err(e) => Err(err(e)),
            }
        }
    }

    impl Phone for Afc {
        fn name(&self) -> &str {
            &self.name
        }

        fn list(&mut self) -> Result<Vec<Photo>> {
            let Afc { rt, afc, root, .. } = self;
            rt.block_on(async {
                let mut photos = Vec::new();
                for kind in Kind::ALL {
                    let dir = format!("{root}/{}", kind.folder());
                    for name in Afc::list_dir(afc, &dir).await? {
                        if Kind::of(&name) != Some(kind) {
                            continue;
                        }
                        let info = afc.get_file_info(format!("{dir}/{name}")).await.map_err(err)?;
                        if info.st_ifmt == "S_IFREG" {
                            photos.push(Photo { kind, name, size: info.size as u64 });
                        }
                    }
                }
                photos.sort();
                Ok(photos)
            })
        }

        fn upload(&mut self, src: &Path, photo: &Photo, progress: &mut dyn FnMut(u64)) -> Result<()> {
            let Afc { rt, afc, root, .. } = self;
            let mut file = std::fs::File::open(src)?;
            let dest = format!("{root}/{}", photo.rel_path());
            let part = format!("{dest}.part");
            rt.block_on(async {
                Afc::make_dirs(afc, root, photo).await;
                let mut fd = afc.open(part.clone(), AfcFopenMode::WrOnly).await.map_err(err)?;
                let mut buf = vec![0; CHUNK];
                let mut sent = 0;
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    fd.write_entire(&buf[..n]).await.map_err(err)?;
                    sent += n as u64;
                    progress(sent);
                }
                fd.close().await.map_err(err)?;
                let size = afc.get_file_info(part.clone()).await.map_err(err)?.size as u64;
                if size != photo.size {
                    let _ = afc.remove(part).await;
                    return Err(Error::Phone(format!("{}: sent {size} of {} bytes", photo.name, photo.size)));
                }
                afc.rename(part, dest).await.map_err(err)
            })
        }
    }
}
