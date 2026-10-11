//! Physical disks opened read-only through the operating system.
//!
//! This is the only module in Rescyo allowed to use `unsafe`, and only for FFI calls that query
//! disk geometry and identity (`ioctl`, `DeviceIoControl`). Data is read with positional reads
//! on a handle that was opened without write access.
//!
//! Integration tests for this module run only on disposable CI runners (ADR-0002).

use std::fs::File;
use std::io;

use crate::image::{classify, read_exact_at};
use crate::{BlockSource, Geometry, ReadError, SourceIdentity, SourceKind};

/// A disk as listed by [`list_disks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    pub path: String,
    pub size: Option<u64>,
    pub logical_sector: Option<u32>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub removable: Option<bool>,
}

/// A whole disk or partition device opened for reading only.
#[derive(Debug)]
pub struct PhysicalDisk {
    file: File,
    geometry: Geometry,
    identity: SourceIdentity,
}

impl PhysicalDisk {
    /// Opens a device path (`/dev/sdb`, `/dev/rdisk3`, `\\.\PhysicalDrive2`) read-only.
    pub fn open(path: &str) -> io::Result<Self> {
        let (file, info) = sys::open(path)?;
        let logical_sector = info.logical_sector.unwrap_or(512);
        if !logical_sector.is_power_of_two() || !(512..=65536).contains(&logical_sector) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "implausible sector size",
            ));
        }
        let size = info
            .size
            .ok_or_else(|| io::Error::other("cannot determine device size"))?;
        let geometry = Geometry {
            size: size - size % u64::from(logical_sector),
            logical_sector,
        };
        let identity = SourceIdentity {
            kind: SourceKind::PhysicalDisk,
            path: path.to_owned(),
            model: info.model,
            serial: info.serial,
        };
        Ok(Self {
            file,
            geometry,
            identity,
        })
    }
}

impl BlockSource for PhysicalDisk {
    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        let ss = u64::from(self.geometry.logical_sector);
        if !crate::is_aligned(offset, buf.len(), ss) {
            return Err(ReadError::Misaligned);
        }
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(ReadError::OutOfBounds)?;
        if end > self.geometry.size {
            return Err(ReadError::OutOfBounds);
        }
        read_exact_at(&self.file, offset, buf).map_err(classify)
    }
}

/// Lists disks visible to the operating system. Never opens anything for writing.
pub fn list_disks() -> io::Result<Vec<DiskInfo>> {
    sys::list()
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn trimmed(s: &str) -> Option<String> {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!t.is_empty()).then(|| t.to_owned())
}

#[cfg(target_os = "linux")]
mod sys {
    use super::{DiskInfo, trimmed};
    use std::fs::{self, File};
    use std::io;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::path::{Path, PathBuf};

    pub fn open(path: &str) -> io::Result<(File, DiskInfo)> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        if !meta.file_type().is_block_device() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a block device"));
        }
        let sys = sysfs_dir(meta.rdev());
        let info = DiskInfo {
            path: path.to_owned(),
            size: read_u64(&sys.join("size")).map(|s| s * 512),
            logical_sector: queue_value(&sys, "logical_block_size").and_then(|v| u32::try_from(v).ok()),
            model: device_attr(&sys, "model"),
            serial: device_attr(&sys, "serial"),
            removable: read_u64(&sys.join("removable")).map(|v| v == 1),
        };
        Ok((file, info))
    }

    pub fn list() -> io::Result<Vec<DiskInfo>> {
        let mut out = Vec::new();
        for entry in fs::read_dir("/sys/block")? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if ["loop", "ram", "zram", "sr", "fd"]
                .iter()
                .any(|p| name.starts_with(p))
            {
                continue;
            }
            let sys = entry.path();
            out.push(DiskInfo {
                path: format!("/dev/{name}"),
                size: read_u64(&sys.join("size")).map(|s| s * 512),
                logical_sector: read_u64(&sys.join("queue/logical_block_size"))
                    .and_then(|v| u32::try_from(v).ok()),
                model: read_str(&sys.join("device/model")),
                serial: read_str(&sys.join("device/serial")),
                removable: read_u64(&sys.join("removable")).map(|v| v == 1),
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn sysfs_dir(rdev: u64) -> PathBuf {
        let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
        let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
        let link = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
        fs::canonicalize(&link).unwrap_or(link)
    }

    /// Queue attributes live on the whole disk; a partition's parent directory is its disk.
    fn queue_value(sys: &Path, name: &str) -> Option<u64> {
        read_u64(&sys.join("queue").join(name))
            .or_else(|| sys.parent().and_then(|p| read_u64(&p.join("queue").join(name))))
    }

    fn device_attr(sys: &Path, name: &str) -> Option<String> {
        read_str(&sys.join("device").join(name))
            .or_else(|| sys.parent().and_then(|p| read_str(&p.join("device").join(name))))
    }

    fn read_str(p: &Path) -> Option<String> {
        fs::read_to_string(p).ok().and_then(|s| trimmed(&s))
    }

    fn read_u64(p: &Path) -> Option<u64> {
        read_str(p).and_then(|s| s.parse().ok())
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use super::DiskInfo;
    use std::fs::{self, File};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::FileTypeExt;

    // _IOR('d', 24, u32) and _IOR('d', 25, u64) from <sys/disk.h>.
    const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
    const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;

    pub fn open(path: &str) -> io::Result<(File, DiskInfo)> {
        let file = File::open(path)?;
        let ft = file.metadata()?.file_type();
        if !ft.is_block_device() && !ft.is_char_device() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a disk device"));
        }
        let fd = file.as_raw_fd();
        let mut block_size: u32 = 0;
        let mut block_count: u64 = 0;
        // SAFETY: `fd` is a valid open descriptor for the lifetime of `file`; each ioctl writes
        // exactly one value of the pointed-to type, as declared in <sys/disk.h>.
        let ok = unsafe {
            libc::ioctl(fd, DKIOCGETBLOCKSIZE, &mut block_size as *mut u32) == 0
                && libc::ioctl(fd, DKIOCGETBLOCKCOUNT, &mut block_count as *mut u64) == 0
        };
        if !ok {
            return Err(io::Error::last_os_error());
        }
        let info = DiskInfo {
            path: path.to_owned(),
            size: block_count.checked_mul(u64::from(block_size)),
            logical_sector: Some(block_size),
            model: None,
            serial: None,
            removable: None,
        };
        Ok((file, info))
    }

    pub fn list() -> io::Result<Vec<DiskInfo>> {
        let mut out = Vec::new();
        for entry in fs::read_dir("/dev")? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let Some(num) = name.strip_prefix("disk") else {
                continue;
            };
            if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let raw = format!("/dev/r{name}");
            let info = open(&raw).map(|(_, i)| i).unwrap_or(DiskInfo {
                path: raw.clone(),
                size: None,
                logical_sector: None,
                model: None,
                serial: None,
                removable: None,
            });
            out.push(info);
        }
        out.sort_by_key(|d| {
            d.path
                .trim_start_matches("/dev/rdisk")
                .parse::<u32>()
                .unwrap_or(u32::MAX)
        });
        Ok(out)
    }
}

#[cfg(windows)]
mod sys {
    use super::{DiskInfo, trimmed};
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
        STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
    };

    pub fn open(path: &str) -> io::Result<(File, DiskInfo)> {
        // Read access only; sharing lets the OS keep the volume mounted while we read it.
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)?;
        let (size, logical_sector) = geometry(&file)?;
        let (model, serial, removable) = descriptor(&file).unwrap_or((None, None, None));
        let info = DiskInfo {
            path: path.to_owned(),
            size: Some(size),
            logical_sector: Some(logical_sector),
            model,
            serial,
            removable,
        };
        Ok((file, info))
    }

    pub fn list() -> io::Result<Vec<DiskInfo>> {
        let mut out = Vec::new();
        for n in 0..64 {
            let path = format!(r"\\.\PhysicalDrive{n}");
            match open(&path) {
                Ok((_, info)) => out.push(info),
                // Access denied still proves the disk exists: list it without details.
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => out.push(DiskInfo {
                    path,
                    size: None,
                    logical_sector: None,
                    model: None,
                    serial: None,
                    removable: None,
                }),
                Err(_) => {}
            }
        }
        Ok(out)
    }

    fn ioctl(file: &File, code: u32, input: &[u8], output: &mut [u8]) -> io::Result<usize> {
        let mut returned: u32 = 0;
        let in_len = u32::try_from(input.len()).map_err(io::Error::other)?;
        let out_len = u32::try_from(output.len()).map_err(io::Error::other)?;
        // SAFETY: the handle is valid for the lifetime of `file`; both buffers are valid for the
        // lengths passed, and the kernel writes at most `out_len` bytes to `output`.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle() as _,
                code,
                if input.is_empty() {
                    std::ptr::null()
                } else {
                    input.as_ptr().cast()
                },
                in_len,
                output.as_mut_ptr().cast(),
                out_len,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(returned as usize)
        }
    }

    fn geometry(file: &File) -> io::Result<(u64, u32)> {
        let mut out = vec![0u8; 256];
        let n = ioctl(file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut out)?;
        // DISK_GEOMETRY_EX starts with DISK_GEOMETRY (24 bytes) followed by DiskSize (8 bytes).
        if n < 32 {
            return Err(io::Error::other("short geometry reply"));
        }
        // DISK_GEOMETRY (24 bytes): BytesPerSector at offset 20; DiskSize (i64) at offset 24.
        let bps = u32::from_le_bytes(
            out.get(20..24)
                .and_then(|s| s.try_into().ok())
                .unwrap_or_default(),
        );
        let size = i64::from_le_bytes(
            out.get(24..32)
                .and_then(|s| s.try_into().ok())
                .unwrap_or_default(),
        );
        Ok((u64::try_from(size).map_err(io::Error::other)?, bps))
    }

    fn descriptor(file: &File) -> io::Result<(Option<String>, Option<String>, Option<bool>)> {
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        // SAFETY: STORAGE_PROPERTY_QUERY is plain old data; viewing it as bytes is sound.
        let input = unsafe {
            std::slice::from_raw_parts(
                (&query as *const STORAGE_PROPERTY_QUERY).cast::<u8>(),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>(),
            )
        };
        let mut out = vec![0u8; 1024];
        let n = ioctl(file, IOCTL_STORAGE_QUERY_PROPERTY, input, &mut out)?;
        let out = out.get(..n).unwrap_or_default();
        // STORAGE_DEVICE_DESCRIPTOR: RemovableMedia u8 @10, VendorIdOffset u32 @12,
        // ProductIdOffset @16, ProductRevisionOffset @20, SerialNumberOffset @24.
        let u32_at = |o: usize| {
            out.get(o..o + 4)
                .and_then(|s| s.try_into().ok())
                .map(u32::from_le_bytes)
        };
        let cstr_at = |off: Option<u32>| -> Option<String> {
            let off = usize::try_from(off?).ok()?;
            if off == 0 {
                return None;
            }
            let tail = out.get(off..)?;
            let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            trimmed(&String::from_utf8_lossy(tail.get(..end)?))
        };
        let vendor = cstr_at(u32_at(12));
        let product = cstr_at(u32_at(16));
        let model = match (vendor, product) {
            (Some(v), Some(p)) => Some(format!("{v} {p}")),
            (v, p) => v.or(p),
        };
        let serial = cstr_at(u32_at(24));
        let removable = out.get(10).map(|&b| b != 0);
        Ok((model, serial, removable))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod sys {
    use super::DiskInfo;
    use std::fs::File;
    use std::io;

    pub fn open(_path: &str) -> io::Result<(File, DiskInfo)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "physical disks are not supported on this OS",
        ))
    }

    pub fn list() -> io::Result<Vec<DiskInfo>> {
        Ok(Vec::new())
    }
}
