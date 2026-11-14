//! The camera, read-only: Ricochet lists and downloads, and never deletes, uploads or changes
//! settings on it.
//!
//! It speaks PTP over USB itself instead of going through libgphoto2, which shows the GR IV as
//! empty: the camera reports `DCIM`'s parent as 0xFFFFFFFF rather than 0, and libgphoto2's
//! generic driver lists the root by looking for parent 0.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use nusb::io::{EndpointRead, EndpointWrite};
use nusb::transfer::{Bulk, Direction, In, Out};

use crate::plan::CameraFile;
use crate::{Error, Result};

pub trait Camera {
    /// The model, as the camera reports it.
    fn name(&self) -> &str;
    /// Every file on every storage, by name.
    fn list(&mut self) -> Result<Vec<CameraFile>>;
    /// Downloads one file to `to`, which must not exist yet, reporting bytes done so far.
    fn download(
        &mut self,
        file: &CameraFile,
        to: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()>;
}

/// The first connected camera, or `None` when there isn't one.
pub fn detect() -> Result<Option<Box<dyn Camera + Send>>> {
    Ptp::detect()
}

const APPLE: u16 = 0x05ac;
const STILL_IMAGE: u8 = 6;
const TIMEOUT: Duration = Duration::from_secs(30);

// Operations, all read-only.
const GET_STORAGE_IDS: u16 = 0x1004;
const GET_OBJECT_HANDLES: u16 = 0x1007;
const GET_OBJECT_INFO: u16 = 0x1008;
const GET_OBJECT: u16 = 0x1009;
const OPEN_SESSION: u16 = 0x1002;
const CLOSE_SESSION: u16 = 0x1003;

const OK: u16 = 0x2001;
const SESSION_ALREADY_OPEN: u16 = 0x201e;
const ASSOCIATION: u16 = 0x3001;

const DATA: u16 = 2;
const RESPONSE: u16 = 3;

fn usb(e: impl std::fmt::Display) -> Error {
    Error::Camera(e.to_string())
}

struct Ptp {
    name: String,
    input: EndpointRead<Bulk>,
    output: EndpointWrite<Bulk>,
    transaction: u32,
}

impl Ptp {
    fn detect() -> Result<Option<Box<dyn Camera + Send>>> {
        // An iPhone also answers as a PTP camera; it's the phone, not a source.
        let found = nusb::list_devices().wait().map_err(usb)?.find_map(|d| {
            let n = d
                .interfaces()
                .find(|i| i.class() == STILL_IMAGE)?
                .interface_number();
            (d.vendor_id() != APPLE).then_some((d, n))
        });
        let Some((info, number)) = found else {
            return Ok(None);
        };
        let name = info.product_string().unwrap_or("Camera").to_owned();
        let device = info.open().wait().map_err(usb)?;
        let interface = device
            .claim_interface(number)
            .wait()
            .map_err(|e| Error::Camera(format!("{name} is in use by another app ({e})")))?;
        let endpoint = |dir| {
            let desc = interface.descriptor()?;
            desc.endpoints()
                .find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == dir)
                .map(|e| e.address())
        };
        let (Some(input), Some(output)) = (endpoint(Direction::In), endpoint(Direction::Out))
        else {
            return Err(Error::Camera(format!("{name} has no PTP endpoints")));
        };
        let input = interface
            .endpoint::<Bulk, In>(input)
            .map_err(usb)?
            .reader(256 * 1024)
            .with_read_timeout(TIMEOUT);
        let output = interface
            .endpoint::<Bulk, Out>(output)
            .map_err(usb)?
            .writer(4096)
            .with_write_timeout(TIMEOUT);
        let mut ptp = Ptp {
            name,
            input,
            output,
            transaction: 0,
        };
        ptp.drain();
        match ptp.run(OPEN_SESSION, &[1], &mut |_| Ok(())) {
            Ok(_) | Err(Response(SESSION_ALREADY_OPEN)) => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Some(Box::new(ptp)))
    }

    /// Drops whatever an earlier, interrupted session left queued on the camera.
    fn drain(&mut self) {
        self.input.set_read_timeout(Duration::from_millis(200));
        let mut buf = vec![0; 256 * 1024];
        while matches!(self.input.read(&mut buf), Ok(n) if n > 0) {}
        self.input.set_read_timeout(TIMEOUT);
    }

    /// One transaction: the command, its data phase if the camera sends one (fed to `data`),
    /// and the response's parameters.
    fn run(
        &mut self,
        code: u16,
        params: &[u32],
        data: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<Vec<u32>, Failure> {
        self.transaction += 1;
        let mut command = Vec::with_capacity(12 + 4 * params.len());
        command.extend((12 + 4 * params.len() as u32).to_le_bytes());
        command.extend(1u16.to_le_bytes());
        command.extend(code.to_le_bytes());
        command.extend(self.transaction.to_le_bytes());
        for p in params {
            command.extend(p.to_le_bytes());
        }
        self.output.write_all(&command).map_err(usb)?;
        self.output.flush_end().map_err(usb)?;

        loop {
            let (kind, code, transaction, payload) = self.read_container(data)?;
            match kind {
                _ if transaction != self.transaction => continue,
                DATA => continue,
                RESPONSE if code == OK => {
                    return Ok(payload
                        .as_chunks()
                        .0
                        .iter()
                        .map(|c| u32::from_le_bytes(*c))
                        .collect());
                }
                RESPONSE => return Err(Response(code)),
                _ => continue,
            }
        }
    }

    /// Reads one container. The payload of a data container for the current transaction streams
    /// to `data`; any other payload is returned.
    /// Containers end with a short USB packet, but some cameras also send the header on its own,
    /// so this keeps reading until the declared length has arrived.
    fn read_container(
        &mut self,
        data: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<(u16, u16, u32, Vec<u8>), Failure> {
        let mut header = Vec::with_capacity(12);
        let mut payload = Vec::new();
        let mut total: u64 = 0;
        let mut chunk = vec![0; 256 * 1024];
        loop {
            // Read, not BufRead: nusb 0.2.9's `consume` on this adapter panics while data remains.
            let mut packet = self.input.until_short_packet();
            loop {
                let n = packet.read(&mut chunk).map_err(usb)?;
                if n == 0 {
                    break;
                }
                let buf = &chunk[..n];
                let head = (12 - header.len()).min(n);
                header.extend_from_slice(&buf[..head]);
                let rest = &buf[head..];
                if !rest.is_empty() {
                    let kind = u16::from_le_bytes([header[4], header[5]]);
                    let transaction = u32::from_le_bytes(header[8..12].try_into().unwrap());
                    if kind == DATA && transaction == self.transaction {
                        data(rest)?;
                    } else {
                        payload.extend_from_slice(rest);
                    }
                }
                total += n as u64;
            }
            packet.consume_end().map_err(usb)?;
            if header.len() == 12 {
                let length = u32::from_le_bytes(header[..4].try_into().unwrap());
                // 0xFFFFFFFF means over 4 GB: the short packet is the only end marker.
                if length == u32::MAX || total >= length as u64 {
                    break;
                }
            }
        }
        let kind = u16::from_le_bytes([header[4], header[5]]);
        let code = u16::from_le_bytes([header[6], header[7]]);
        let transaction = u32::from_le_bytes(header[8..12].try_into().unwrap());
        Ok((kind, code, transaction, payload))
    }

    fn fetch(&mut self, code: u16, params: &[u32]) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.run(code, params, &mut |b| {
            bytes.extend_from_slice(b);
            Ok(())
        })?;
        Ok(bytes)
    }

    fn handles(&mut self, code: u16, params: &[u32]) -> Result<Vec<u32>> {
        let bytes = self.fetch(code, params)?;
        let mut r = Reader(&bytes);
        let count = r.u32()?;
        (0..count).map(|_| r.u32()).collect()
    }
}

impl Drop for Ptp {
    fn drop(&mut self) {
        let _ = self.run(CLOSE_SESSION, &[], &mut |_| Ok(()));
    }
}

impl Camera for Ptp {
    fn name(&self) -> &str {
        &self.name
    }

    fn list(&mut self) -> Result<Vec<CameraFile>> {
        let mut files = Vec::new();
        for storage in self.handles(GET_STORAGE_IDS, &[])? {
            // Parent 0 asks for every object on the storage, whatever folder it's in.
            for id in self.handles(GET_OBJECT_HANDLES, &[storage, 0, 0])? {
                let info = self.fetch(GET_OBJECT_INFO, &[id])?;
                let mut r = Reader(&info);
                r.skip(4)?;
                let format = r.u16()?;
                r.skip(2)?;
                let size = r.u32()?;
                r.skip(2 + 4 * 7 + 2 + 4 + 4)?;
                let name = r.string()?;
                if format == ASSOCIATION {
                    continue;
                }
                // PTP's size field tops out at 4 GB, and the GR IV has no MTP property to ask for
                // more, so a longer clip's size is unknown.
                let size = if size == u32::MAX { 0 } else { size as u64 };
                files.push(CameraFile { id, name, size });
            }
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(files)
    }

    fn download(
        &mut self,
        file: &CameraFile,
        to: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create_new(to)?);
        let mut done = 0;
        self.run(GET_OBJECT, &[file.id], &mut |bytes| {
            out.write_all(bytes)?;
            done += bytes.len() as u64;
            progress(done);
            Ok(())
        })?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    }
}

/// Why a transaction failed: the camera said no, or something else went wrong.
enum Failure {
    Response(u16),
    Other(Error),
}
use Failure::Response;

impl From<Error> for Failure {
    fn from(e: Error) -> Failure {
        Failure::Other(e)
    }
}

impl From<Failure> for Error {
    fn from(f: Failure) -> Error {
        match f {
            Failure::Response(code) => {
                Error::Camera(format!("camera refused with code {code:#06x}"))
            }
            Failure::Other(e) => e,
        }
    }
}

/// Little-endian fields from a PTP dataset.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.0.len() < n {
            return Err(Error::Camera("short reply from the camera".into()));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(drop)
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    /// A count of UTF-16 units, the terminating NUL included, then the units.
    fn string(&mut self) -> Result<String> {
        let count = self.take(1)?[0] as usize;
        let units: Vec<u16> = self
            .take(count * 2)?
            .as_chunks()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        Ok(String::from_utf16_lossy(
            units.strip_suffix(&[0]).unwrap_or(&units),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_ptp_string() {
        let bytes = [4, b'D', 0, b'C', 0, b'I', 0, 0, 0, 0xff];
        let mut r = Reader(&bytes);
        assert_eq!(r.string().unwrap(), "DCI");
        assert_eq!(r.0, [0xff]);
    }
}
