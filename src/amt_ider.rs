// bobcat-cli: Intel AMT IDE-R (IDER), virtual media over the redirection protocol.
//
// The machine's BIOS or OS sees a CD/DVD drive and/or a floppy drive whose contents come from files
// on this computer, served read-only over the AMT redirection channel (port 16994, or 16995 with TLS).
// The AMT firmware sends SCSI commands (as IDE-R messages) and this module answers them from the
// image files. A port of MeshCommander's CreateAmtRemoteIder (0.9.5-a, `public/default.htm`), which
// is Apache-2.0; the SCSI answers and the constant tables follow it closely.
// License: CC BY-NC-SA 4.0 (LICENSE.md).

use crate::amt_redir::{redir_connect, Pin};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

// Mode sense and GET_CONFIGURATION tables (generated from MeshCommander's source).
const MODESENCE_LS120DISK_PAGE_ARRAY: &[u8] = &[0x00, 0x26, 0x31, 0x80, 0x00, 0x00, 0x00, 0x00, 0x05, 0x1E, 0x10, 0xA9, 0x08, 0x20, 0x02, 0x00, 0x03, 0xC3, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xD0, 0x00, 0x00];
const MODESENCE_3F_LS120_ARRAY: &[u8] = &[0x00, 0x5c, 0x24, 0x80, 0x00, 0x00, 0x00, 0x00, 0x01, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x03, 0x16, 0x00, 0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xa0, 0x00, 0x00, 0x00, 0x05, 0x1E, 0x10, 0xA9, 0x08, 0x20, 0x02, 0x00, 0x03, 0xC3, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xD0, 0x00, 0x00, 0x08, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x06, 0x00, 0x00, 0x00, 0x11, 0x24, 0x31];
const MODESENCE_FLOPPYDISK_PAGE_ARRAY: &[u8] = &[0x00, 0x26, 0x24, 0x80, 0x00, 0x00, 0x00, 0x00, 0x05, 0x1E, 0x04, 0xB0, 0x02, 0x12, 0x02, 0x00, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xD0, 0x00, 0x00];
const MODESENCE_3F_FLOPPY_ARRAY: &[u8] = &[0x00, 0x5c, 0x24, 0x80, 0x00, 0x00, 0x00, 0x00, 0x01, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x03, 0x16, 0x00, 0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xa0, 0x00, 0x00, 0x00, 0x05, 0x1e, 0x04, 0xb0, 0x02, 0x12, 0x02, 0x00, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xd0, 0x00, 0x00, 0x08, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x06, 0x00, 0x00, 0x00, 0x11, 0x24, 0x31];
const MODESENCE_CD_1A_ARRAY: &[u8] = &[0x00, 0x12, 0x01, 0x80, 0x00, 0x00, 0x00, 0x00, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
const MODESENCE_CD_1D_ARRAY: &[u8] = &[0x00, 0x12, 0x01, 0x80, 0x00, 0x00, 0x00, 0x00, 0x1D, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
const MODESENCE_CD_2A_ARRAY: &[u8] = &[0x00, 0x20, 0x01, 0x80, 0x00, 0x00, 0x00, 0x00, 0x2a, 0x18, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
const MODESENCE_3F_CD_ARRAY: &[u8] = &[0x00, 0x28, 0x01, 0x80, 0x00, 0x00, 0x00, 0x00, 0x01, 0x06, 0x00, 0xff, 0x00, 0x00, 0x00, 0x00, 0x2a, 0x18, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
const CD_CONFIGARRAYPROFILELIST: &[u8] = &[0x00, 0x00, 0x03, 0x04, 0x00, 0x08, 0x01, 0x00];
const CD_CONFIGARRAYCORE: &[u8] = &[0x00, 0x01, 0x03, 0x04, 0x00, 0x00, 0x00, 0x02];
const CD_MORPHING: &[u8] = &[0x00, 0x02, 0x03, 0x04, 0x00, 0x00, 0x00, 0x00];
const CD_CONFIGARRAYREMOVABLE: &[u8] = &[0x00, 0x03, 0x03, 0x04, 0x29, 0x00, 0x00, 0x02];
const CD_CONFIGARRAYRANDOM: &[u8] = &[0x00, 0x10, 0x01, 0x08, 0x00, 0x00, 0x08, 0x00, 0x00, 0x01, 0x00, 0x00];
const CD_READ: &[u8] = &[0x00, 0x1E, 0x03, 0x00];
const CD_POWERMANAGEMENT: &[u8] = &[0x01, 0x00, 0x03, 0x00];
const CD_TIMEOUT: &[u8] = &[0x01, 0x05, 0x03, 0x00];
const MODESENCE_FLOPPYERROR_RECOVERY_ARRAY: &[u8] = &[0x00, 0x12, 0x24, 0x80, 0x00, 0x00, 0x00, 0x00, 0x01, 0x0A, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
const MODESENCE_LS120ERROR_RECOVERY_ARRAY: &[u8] = &[0x00, 0x12, 0x31, 0x80, 0x00, 0x00, 0x00, 0x00, 0x01, 0x0A, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
const MODESENCE_CDERROR_RECOVERY_ARRAY: &[u8] = &[0x00, 0x0E, 0x01, 0x80, 0x00, 0x00, 0x00, 0x00, 0x01, 0x06, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00];

const DEV_FLOPPY: u8 = 0xA0;
const DEV_CDDVD: u8 = 0xB0;

/// When the device should start using the redirection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IderStart {
    /// On the next boot
    OnReboot = 0,
    Graceful = 1,
    Now = 2,
}

/// What the session has done so far (read by the caller while the session runs).
#[derive(Default)]
pub struct IderStats {
    pub bytes_from_amt: AtomicU64,
    pub bytes_to_amt: AtomicU64,
    /// Bytes of image data sent, CD and floppy
    pub image_bytes: AtomicU64,
}

struct Media {
    file: File,
    size: u64,
}

impl Media {
    fn open(path: &Path, block: u64, what: &str) -> Result<Media, String> {
        let file = File::open(path).map_err(|e| format!("cannot open {what} image {}: {e}", path.display()))?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        if size == 0 || size % block != 0 {
            return Err(format!("{what} image {} is {size} bytes, not a whole number of {block}-byte blocks", path.display()));
        }
        Ok(Media { file, size })
    }

    fn blocks(&self, block: u64) -> u64 {
        self.size / block
    }
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn rd_be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn rd_be16(b: &[u8], at: usize) -> usize {
    u16::from_be_bytes([b[at], b[at + 1]]) as usize
}

struct Session {
    s: TcpStream,
    out_seq: u32,
    in_seq: u32,
    acc: Vec<u8>,
    read_buffer: usize,
    floppy: Option<Media>,
    cdrom: Option<Media>,
    floppy_ready: bool,
    cdrom_ready: bool,
    start: IderStart,
    stats: Arc<IderStats>,
    opened: bool,
}

impl Session {
    fn send_command(&mut self, cmd: u8, data: &[u8], completed: bool, dma: bool) -> Result<(), String> {
        let mut attributes = if cmd > 50 && completed { 2 } else { 0 };
        if dma {
            attributes += 1;
        }
        let mut m = vec![cmd, 0, 0, attributes];
        m.extend_from_slice(&self.out_seq.to_le_bytes());
        self.out_seq = self.out_seq.wrapping_add(1);
        m.extend_from_slice(data);
        self.stats.bytes_to_amt.fetch_add(m.len() as u64, Ordering::Relaxed);
        self.s.write_all(&m).map_err(|e| format!("sending to the AMT device: {e}"))
    }

    /// CommandEndResponse (SCSI sense). `error` picks the shape MeshCommander uses for "good" (1) and
    /// "sense data" (0).
    fn end_response(&mut self, error: bool, sense: u8, device: u8, asc: u8, asq: u8) -> Result<(), String> {
        let mut d = vec![0u8; 12];
        if error {
            d.extend_from_slice(&[0xc5, 0, 3, 0, 0, 0, device, 0x50, 0, 0, 0]);
        } else {
            d.extend_from_slice(&[0x87, sense << 4, 3, 0, 0, 0, device, 0x51, sense, asc, asq]);
        }
        self.send_command(0x51, &d, true, false)
    }

    /// DataToHost (SCSI read data)
    fn data_to_host(&mut self, device: u8, completed: bool, data: &[u8], dma: bool) -> Result<(), String> {
        let dmalen = if dma { 0 } else { data.len() };
        let mut d = vec![0, (data.len() & 0xff) as u8, (data.len() >> 8) as u8, 0, if dma { 0xb4 } else { 0xb5 }, 0, 2, 0, (dmalen & 0xff) as u8, (dmalen >> 8) as u8, device, 0x58];
        if completed {
            d.extend_from_slice(&[0x85, 0, 3, 0, 0, 0, device, 0x50, 0, 0, 0, 0, 0, 0]);
        } else {
            d.extend_from_slice(&[0; 14]);
        }
        d.extend_from_slice(data);
        self.send_command(0x54, &d, completed, dma)
    }

    fn get_data_from_host(&mut self, device: u8, chunk: usize) -> Result<(), String> {
        let mut d = vec![0, (chunk & 0xff) as u8, (chunk >> 8) as u8, 0, 0xb5, 0, 0, 0, (chunk & 0xff) as u8, (chunk >> 8) as u8, device, 0x58];
        d.extend_from_slice(&[0; 11]);
        self.send_command(0x52, &d, false, false)
    }

    fn features(&mut self, kind: u8, data: &[u8]) -> Result<(), String> {
        let mut d = vec![kind];
        d.extend_from_slice(data);
        self.send_command(0x48, &d, false, false)
    }

    fn toggle_start(&mut self) -> Result<(), String> {
        let v: u32 = 0x01 + match self.start {
            IderStart::OnReboot => 0x08,
            IderStart::Graceful => 0x10,
            IderStart::Now => 0x18,
        };
        self.features(3, &v.to_le_bytes())
    }

    /// Handle one complete message at the front of the buffer; returns its length, or 0 if it is not complete yet.
    fn process_one(&mut self) -> Result<usize, String> {
        if self.acc.len() < 8 {
            return Ok(0);
        }
        let a = self.acc.clone();
        match a[0] {
            0x41 => {
                if a.len() < 30 {
                    return Ok(0);
                }
                let len = a[29] as usize;
                if a.len() < 30 + len {
                    return Ok(0);
                }
                let readbfr = u16::from_le_bytes([a[16], a[17]]) as usize;
                let writebfr = u16::from_le_bytes([a[18], a[19]]) as usize;
                let proto = a[21];
                if std::env::var_os("BOBCAT_DEBUG").is_some() {
                    eprintln!("[ider] session open: version {}.{}, firmware {}.{}, read buffer {readbfr}, write buffer {writebfr}, protocol {proto}", a[8], a[9], a[10], a[11]);
                }
                if proto != 0 {
                    return Err(format!("the device uses IDER protocol {proto}, which is not supported"));
                }
                if readbfr == 0 || readbfr > 8192 || writebfr > 8192 {
                    return Err(format!("the device announced odd IDER buffer sizes (read {readbfr}, write {writebfr})"));
                }
                self.read_buffer = readbfr;
                self.toggle_start()?;
                self.opened = true;
                Ok(30 + len)
            }
            0x43 => Err("the device closed the IDER session".into()),
            0x44 => {
                self.send_command(0x45, &[], false, false)?;
                Ok(8)
            }
            0x45 | 0x4b => Ok(8),
            0x46 => {
                if a.len() < 9 {
                    return Ok(0);
                }
                self.send_command(0x47, &[], false, false)?; // reset occurred: answered at once
                Ok(9)
            }
            0x49 => {
                if a.len() < 13 {
                    return Ok(0);
                }
                let kind = a[8];
                let value = u32::from_le_bytes([a[9], a[10], a[11], a[12]]);
                if std::env::var_os("BOBCAT_DEBUG").is_some() {
                    eprintln!("[ider] status data: type {kind}, value {value}");
                }
                if kind == 1 && value & 1 != 0 {
                    self.toggle_start()?;
                }
                Ok(13)
            }
            0x4a => {
                if a.len() < 11 {
                    return Ok(0);
                }
                if std::env::var_os("BOBCAT_DEBUG").is_some() {
                    eprintln!("[ider] device reported an error ({})", a[8]);
                }
                Ok(11)
            }
            0x50 => {
                if a.len() < 28 {
                    return Ok(0);
                }
                let flags = a[14];
                let dev = if flags & 0x10 != 0 { DEV_CDDVD } else { DEV_FLOPPY };
                let mut cdb = [0u8; 12];
                cdb.copy_from_slice(&a[16..28]);
                self.scsi(dev, &cdb, a[9], flags)?;
                Ok(28)
            }
            0x53 => {
                if a.len() < 14 {
                    return Ok(0);
                }
                let len = u16::from_le_bytes([a[9], a[10]]) as usize;
                if a.len() < 14 + len {
                    return Ok(0);
                }
                let mut d = vec![0u8; 12];
                d.extend_from_slice(&[0x87, 0x70, 0x03, 0, 0, 0, 0xa0, 0x51, 0x07, 0x27, 0x00]);
                self.send_command(0x51, &d, true, false)?;
                Ok(14 + len)
            }
            c => Err(format!("the device sent an unknown IDER command (0x{c:02x})")),
        }
    }

    fn media(&self, dev: u8) -> Option<&Media> {
        if dev == DEV_FLOPPY { self.floppy.as_ref() } else { self.cdrom.as_ref() }
    }

    fn block(dev: u8) -> u64 {
        if dev == DEV_FLOPPY { 512 } else { 2048 }
    }

    fn scsi(&mut self, dev: u8, cdb: &[u8; 12], fr: u8, flags: u8) -> Result<(), String> {
        let dma = fr & 1 != 0;
        let debug = std::env::var_os("BOBCAT_DEBUG").is_some();
        if debug {
            eprintln!("[ider] SCSI {:02x} on {:02x}", cdb[0], dev);
        }
        match cdb[0] {
            0x00 => {
                // TEST_UNIT_READY
                if self.media(dev).is_none() {
                    return self.end_response(true, 0x02, dev, 0x3a, 0x00);
                }
                let ready = if dev == DEV_FLOPPY { &mut self.floppy_ready } else { &mut self.cdrom_ready };
                if !*ready {
                    *ready = true;
                    return self.end_response(true, 0x06, dev, 0x28, 0x00); // switch to ready
                }
                self.end_response(true, 0x00, dev, 0x00, 0x00)
            }
            0x08 => {
                let lba = (((cdb[1] & 0x1f) as u32) << 16) + ((cdb[2] as u32) << 8) + cdb[3] as u32;
                let len = if cdb[4] == 0 { 256 } else { cdb[4] as u32 };
                self.send_disk_data(dev, lba as u64, len as u64, dma)
            }
            0x0a => self.end_response(true, 0x02, dev, 0x3a, 0x00), // WRITE_6: not supported
            0x1a => {
                if cdb[2] == 0x3f && cdb[3] == 0x00 {
                    if self.media(dev).is_none() {
                        return self.end_response(true, 0x02, dev, 0x3a, 0x00);
                    }
                    let (a, b) = if dev == DEV_FLOPPY { (0x00, 0x80) } else { (0x05, 0x80) };
                    return self.data_to_host(dev, true, &[0, a, b, 0], dma);
                }
                self.end_response(true, 0x05, dev, 0x24, 0x00)
            }
            0x1b => self.end_response(true, 0, dev, 0, 0), // START_STOP (eject)
            0x1e => {
                if self.media(dev).is_none() {
                    return self.end_response(true, 0x02, dev, 0x3a, 0x00);
                }
                self.end_response(true, 0x00, dev, 0x00, 0x00)
            }
            0x23 => {
                // READ_FORMAT_CAPACITIES
                if self.media(dev).is_none() {
                    return self.end_response(false, 0x05, dev, 0x24, 0x00);
                }
                let mut d = be32(8).to_vec();
                d.extend_from_slice(&[0x00, 0x00, 0x0b, 0x40, 0x02, 0x00, 0x02, 0x00]);
                self.data_to_host(dev, true, &d, dma)
            }
            0x25 => {
                // READ_CAPACITY
                let Some(m) = self.media(dev) else { return self.end_response(false, 0x02, dev, 0x3a, 0x00) };
                let last = (m.blocks(Self::block(dev)) - 1) as u32;
                let mut d = be32(last).to_vec();
                d.extend_from_slice(&[0, 0, if dev == DEV_CDDVD { 0x08 } else { 0x02 }, 0]);
                self.data_to_host(flags, true, &d, dma)
            }
            0x28 => {
                let (lba, len) = (rd_be32(cdb, 2) as u64, rd_be16(cdb, 7) as u64);
                self.send_disk_data(dev, lba, len, dma)
            }
            0x2a | 0x2e => {
                let len = rd_be16(cdb, 7);
                self.get_data_from_host(dev, 512 * len)
            }
            0x43 => {
                // READ_TOC
                let msf = cdb[1] & 0x02 != 0;
                let mut format = cdb[2] & 0x07;
                if format == 0 {
                    format = cdb[9] >> 6;
                }
                if dev == DEV_FLOPPY {
                    return self.end_response(true, 0x05, dev, 0x20, 0x00);
                }
                if format == 1 {
                    self.data_to_host(dev, true, &[0x00, 0x0a, 0x01, 0x01, 0x00, 0x14, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], dma)
                } else if format == 0 {
                    if msf {
                        self.data_to_host(dev, true, &[0x00, 0x12, 0x01, 0x01, 0x00, 0x14, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x14, 0xaa, 0x00, 0x00, 0x00, 0x34, 0x13], dma)
                    } else {
                        self.data_to_host(dev, true, &[0x00, 0x12, 0x01, 0x01, 0x00, 0x14, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x14, 0xaa, 0x00, 0x00, 0x00, 0x00, 0x00], dma)
                    }
                } else {
                    Ok(())
                }
            }
            0x46 => {
                // GET_CONFIGURATION
                let sendall = cdb[1] != 2;
                let first = rd_be16(cdb, 2);
                let buflen = rd_be16(cdb, 7);
                if buflen == 0 {
                    let mut d = be32(0x003c).to_vec();
                    d.extend_from_slice(&be32(0x0008));
                    return self.data_to_host(dev, true, &d, dma);
                }
                let mut r: Vec<u8> = be32(0x0008).to_vec();
                if first == 0 {
                    r.extend_from_slice(CD_CONFIGARRAYPROFILELIST);
                }
                for (code, table) in [(0x1usize, CD_CONFIGARRAYCORE), (0x2, CD_MORPHING), (0x3, CD_CONFIGARRAYREMOVABLE), (0x10, CD_CONFIGARRAYRANDOM), (0x1E, CD_READ), (0x100, CD_POWERMANAGEMENT), (0x105, CD_TIMEOUT)] {
                    if first == code || (sendall && first < code) {
                        r.extend_from_slice(table);
                    }
                }
                let mut d = be32(r.len() as u32).to_vec();
                d.extend_from_slice(&r);
                d.truncate(buflen);
                self.data_to_host(dev, true, &d, dma)
            }
            0x4a => {
                // GET_EVENT_STATUS_NOTIFICATION
                if cdb[1] != 0x01 && cdb[4] != 0x10 {
                    return self.end_response(true, 0x05, dev, 0x26, 0x01);
                }
                let present = if self.media(dev).is_some() { 0x02 } else { 0x00 };
                self.data_to_host(dev, true, &[0x00, present, 0x80, 0x00], dma)
            }
            0x4c => {
                let mut d = vec![0u8; 12];
                d.extend_from_slice(&[0x87, 0x50, 0x03, 0x00, 0x00, 0x00, 0xb0, 0x51, 0x05, 0x20, 0x00]);
                self.send_command(0x51, &d, true, false)
            }
            0x51 => self.end_response(false, 0x05, dev, 0x20, 0x00), // READ_DISC_INFO
            0x55 => self.end_response(true, 0x05, dev, 0x20, 0x00),  // MODE_SELECT_10
            0x5a => {
                // MODE_SENSE_10
                let buflen = rd_be16(cdb, 7);
                if buflen == 0 {
                    let mut d = be32(0x003c).to_vec();
                    d.extend_from_slice(&be32(0x0008));
                    return self.data_to_host(dev, true, &d, dma);
                }
                let sectors = self.media(dev).map(|m| m.size / Self::block(dev)).unwrap_or(0);
                let small = sectors <= 0xb40;
                let floppy = dev == DEV_FLOPPY;
                let r: Option<&[u8]> = match cdb[2] & 0x3f {
                    0x01 => Some(if floppy { if small { MODESENCE_FLOPPYERROR_RECOVERY_ARRAY } else { MODESENCE_LS120ERROR_RECOVERY_ARRAY } } else { MODESENCE_CDERROR_RECOVERY_ARRAY }),
                    0x05 if floppy => Some(if small { MODESENCE_FLOPPYDISK_PAGE_ARRAY } else { MODESENCE_LS120DISK_PAGE_ARRAY }),
                    0x3f => Some(if floppy { if small { MODESENCE_3F_FLOPPY_ARRAY } else { MODESENCE_3F_LS120_ARRAY } } else { MODESENCE_3F_CD_ARRAY }),
                    0x1A if !floppy => Some(MODESENCE_CD_1A_ARRAY),
                    0x1D if !floppy => Some(MODESENCE_CD_1D_ARRAY),
                    0x2A if !floppy => Some(MODESENCE_CD_2A_ARRAY),
                    _ => None,
                };
                match r {
                    Some(d) => self.data_to_host(dev, true, d, dma),
                    None => self.end_response(false, 0x05, dev, 0x20, 0x00),
                }
            }
            _ => self.end_response(false, 0x05, dev, 0x20, 0x00),
        }
    }

    /// READ_6/READ_10: the blocks go to the device in pieces of at most the read buffer it announced.
    fn send_disk_data(&mut self, dev: u8, lba: u64, len: u64, dma: bool) -> Result<(), String> {
        let block = Self::block(dev);
        let blocks = self.media(dev).map(|m| m.blocks(block)).unwrap_or(0);
        if lba + len > blocks {
            return self.end_response(true, 0x05, dev, 0x21, 0x00);
        }
        if len == 0 {
            return self.end_response(true, 0x00, dev, 0x00, 0x00);
        }
        let (mut pos, mut left) = (lba * block, len * block);
        let mut buf = vec![0u8; self.read_buffer];
        while left > 0 {
            let n = left.min(self.read_buffer as u64) as usize;
            {
                let m = if dev == DEV_FLOPPY { self.floppy.as_mut() } else { self.cdrom.as_mut() }.ok_or("no media")?;
                m.file.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
                m.file.read_exact(&mut buf[..n]).map_err(|e| format!("reading the image: {e}"))?;
            }
            left -= n as u64;
            pos += n as u64;
            self.stats.image_bytes.fetch_add(n as u64, Ordering::Relaxed);
            let chunk = buf[..n].to_vec();
            self.data_to_host(dev, left == 0, &chunk, dma)?;
        }
        Ok(())
    }
}

/// Run an IDER session until `stop` is set or the device ends it. `ready` gets Ok(()) once the device
/// has accepted the session (so the caller can go on with power and boot actions), or the error that
/// stopped it before that. With TLS the session goes through a local relay (`pin` checks the
/// device's certificate).
#[allow(clippy::too_many_arguments)]
pub fn run(
    ip: &str,
    port: u16,
    user: &str,
    pass: &str,
    pin: Option<Pin>,
    floppy: Option<&Path>,
    cdrom: Option<&Path>,
    start: IderStart,
    stop: Arc<AtomicBool>,
    stats: Arc<IderStats>,
    ready: Option<std::sync::mpsc::Sender<Result<(), String>>>,
) -> Result<(), String> {
    let result = (|| -> Result<(), String> {
        if floppy.is_none() && cdrom.is_none() {
            return Err("IDER needs a CD/DVD image (.iso) or a floppy image (.img)".into());
        }
        let floppy = floppy.map(|p| Media::open(p, 512, "floppy")).transpose()?;
        let cdrom = cdrom.map(|p| Media::open(p, 2048, "CD/DVD")).transpose()?;
        let s = redir_connect(ip, port, b"IDER", user, pass, pin)?;
        s.set_read_timeout(Some(Duration::from_millis(300))).ok();
        let mut sess = Session { s, out_seq: 0, in_seq: 0, acc: Vec::new(), read_buffer: 0, floppy, cdrom, floppy_ready: false, cdrom_ready: false, start, stats: stats.clone(), opened: false };
        // OPEN_SESSION: receive timeout, transmit timeout, heartbeat (ms) and the protocol version
        let mut open = Vec::new();
        for v in [30000u16, 0, 20000] {
            open.extend_from_slice(&v.to_le_bytes());
        }
        open.extend_from_slice(&1u32.to_le_bytes());
        sess.send_command(0x40, &open, false, false)?;
        let mut announced = false;
        let mut buf = vec![0u8; 16384];
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            match sess.s.read(&mut buf) {
                Ok(0) => return Err("the device closed the IDER connection".into()),
                Ok(n) => {
                    stats.bytes_from_amt.fetch_add(n as u64, Ordering::Relaxed);
                    sess.acc.extend_from_slice(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("the IDER connection ended ({e})")),
            }
            loop {
                let len = sess.process_one()?;
                if len == 0 {
                    break;
                }
                let seq = u32::from_le_bytes([sess.acc[4], sess.acc[5], sess.acc[6], sess.acc[7]]);
                if seq != sess.in_seq {
                    return Err(format!("the device's IDER messages are out of sequence ({seq}, expected {})", sess.in_seq));
                }
                sess.in_seq += 1;
                sess.acc.drain(..len);
            }
            if sess.opened && !announced {
                announced = true;
                if let Some(r) = &ready {
                    let _ = r.send(Ok(()));
                }
            }
        }
    })();
    if let (Err(e), Some(r)) = (&result, &ready) {
        let _ = r.send(Err(e.clone()));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A session over a loopback socket whose far end the test reads.
    fn session(image: &Path) -> (Session, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let near = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (far, _) = l.accept().unwrap();
        far.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let s = Session {
            s: near, out_seq: 0, in_seq: 0, acc: Vec::new(), read_buffer: 2048,
            floppy: None, cdrom: Some(Media::open(image, 2048, "CD/DVD").unwrap()),
            floppy_ready: false, cdrom_ready: false, start: IderStart::OnReboot,
            stats: Arc::new(IderStats::default()), opened: false,
        };
        (s, far)
    }

    /// An IDE-R "command written" message (0x50) carrying a CDB for the CD/DVD drive.
    fn scsi_msg(seq: u32, cdb: [u8; 12]) -> Vec<u8> {
        let mut m = vec![0x50, 0, 0, 0];
        m.extend_from_slice(&seq.to_le_bytes());
        m.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0x10, 0]); // offsets 8..16: byte 14 has the CD/DVD bit
        m.extend_from_slice(&cdb);
        m
    }

    #[test]
    fn read_10_returns_the_image_blocks() {
        let path = std::env::temp_dir().join("bobcat-ider-test.iso");
        let image: Vec<u8> = (0..4 * 2048).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &image).unwrap();
        let (mut sess, mut far) = session(&path);
        // READ_10, lba 1, 2 blocks
        let mut cdb = [0u8; 12];
        cdb[0] = 0x28;
        cdb[2..6].copy_from_slice(&1u32.to_be_bytes());
        cdb[7..9].copy_from_slice(&2u16.to_be_bytes());
        sess.acc = scsi_msg(0, cdb);
        let n = sess.process_one().unwrap();
        assert_eq!(n, 28);
        // two DataToHost messages of 2048 bytes each (the read buffer), the last one completed
        let mut got = Vec::new();
        for i in 0..2 {
            let mut head = [0u8; 8 + 26];
            far.read_exact(&mut head).unwrap();
            assert_eq!(head[0], 0x54);
            assert_eq!(u32::from_le_bytes([head[4], head[5], head[6], head[7]]), i);
            let len = head[9] as usize + ((head[10] as usize) << 8);
            assert_eq!(len, 2048);
            let mut data = vec![0u8; len];
            far.read_exact(&mut data).unwrap();
            got.extend_from_slice(&data);
        }
        assert_eq!(got, image[2048..3 * 2048]);
        // past the end: an error end response
        cdb[2..6].copy_from_slice(&3u32.to_be_bytes());
        sess.acc = scsi_msg(1, cdb);
        sess.process_one().unwrap();
        let mut resp = [0u8; 8 + 23];
        far.read_exact(&mut resp).unwrap();
        assert_eq!(resp[0], 0x51);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_capacity_reports_the_last_block() {
        let path = std::env::temp_dir().join("bobcat-ider-test2.iso");
        std::fs::write(&path, vec![0u8; 5 * 2048]).unwrap();
        let (mut sess, mut far) = session(&path);
        let mut cdb = [0u8; 12];
        cdb[0] = 0x25;
        sess.acc = scsi_msg(0, cdb);
        sess.process_one().unwrap();
        let mut m = [0u8; 8 + 26 + 8];
        far.read_exact(&mut m).unwrap();
        assert_eq!(m[0], 0x54);
        assert_eq!(&m[34..38], &4u32.to_be_bytes()); // last block = 4
        assert_eq!(&m[38..42], &[0, 0, 0x08, 0]); // 2048-byte blocks
        let _ = std::fs::remove_file(&path);
    }
}
