//! Which physical devices a path or a Source lives on, and what a Recovery Target looks like.
//!
//! Used by the write-isolation check (ADR-0003): a Recovery may only write to a Target whose
//! devices are disjoint from the Source's. Anything that cannot be resolved is reported as
//! unknown, and the caller refuses it by default.
//!
//! Tested only on disposable CI runners (ADR-0002).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::{SourceIdentity, SourceKind};

/// Platform-specific id of a whole physical disk (`linux:8:16`, `macos:disk4`,
/// `windows:PhysicalDrive2`).
pub type DeviceId = String;

/// Where a path ends up physically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// On these local whole disks.
    Local(BTreeSet<DeviceId>),
    /// On a network file system: never the local Source.
    Remote(String),
    /// Could not be determined (FUSE, unusual stacks, missing permissions).
    Unknown(String),
}

/// Facts about a Target file system that the Preflight needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TargetInfo {
    /// File-system type name as the OS reports it, lower-case (`fat32`, `msdos`, `exfat`, …).
    pub fs: Option<String>,
    pub free_bytes: Option<u64>,
}

impl TargetInfo {
    /// Largest file the Target file system can hold, if it has a small limit.
    pub fn max_file_size(&self) -> Option<u64> {
        match self.fs.as_deref()? {
            "fat" | "fat12" | "fat16" | "fat32" | "vfat" | "msdos" => Some(u64::from(u32::MAX)),
            _ => None,
        }
    }

    /// Whether names must follow Windows rules because the Target file system does.
    pub fn windows_names(&self) -> bool {
        matches!(
            self.fs.as_deref(),
            Some(
                "fat"
                    | "fat12"
                    | "fat16"
                    | "fat32"
                    | "vfat"
                    | "msdos"
                    | "exfat"
                    | "ntfs"
                    | "ntfs3"
                    | "fuseblk"
            )
        )
    }

    /// Whether names that differ only in case collide.
    pub fn case_insensitive(&self) -> bool {
        cfg!(any(windows, target_os = "macos")) || self.windows_names()
    }
}

/// Resolves the devices holding `path` (or its nearest existing ancestor).
pub fn placement_of_path(path: &Path) -> Placement {
    match existing_ancestor(path) {
        Some(p) => sys::placement_of_path(&p),
        None => Placement::Unknown(format!("{} has no existing ancestor", path.display())),
    }
}

/// Resolves the devices a Source reads from. Images live on whatever holds the file, which is
/// fine to write to (ADR-0003), so they resolve to no device.
pub fn placement_of_source(id: &SourceIdentity) -> Placement {
    match id.kind {
        SourceKind::PhysicalDisk => sys::placement_of_device(&id.path),
        SourceKind::Image | SourceKind::Test => Placement::Local(BTreeSet::new()),
    }
}

/// Free space and file-system type of the volume holding `path`.
pub fn target_info(path: &Path) -> TargetInfo {
    existing_ancestor(path)
        .map(|p| sys::target_info(&p))
        .unwrap_or_default()
}

fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(path).ok()?;
    abs.ancestors().find(|p| p.exists()).map(Path::to_path_buf)
}

#[allow(dead_code)]
fn unknown(e: &io::Error) -> Placement {
    Placement::Unknown(e.to_string())
}

#[cfg(target_os = "linux")]
mod sys {
    use super::{Placement, TargetInfo, unknown};
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    fn split(dev: u64) -> (u64, u64) {
        (
            ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff),
            (dev & 0xff) | ((dev >> 12) & !0xff),
        )
    }

    /// Whole disks behind a sysfs block directory, following partitions and dm/md slaves.
    fn disks(sys: &Path, depth: u32, out: &mut BTreeSet<String>) -> bool {
        if depth > 16 {
            return false;
        }
        let slaves = sys.join("slaves");
        if let Ok(rd) = fs::read_dir(&slaves)
            && let entries = rd.filter_map(Result::ok).collect::<Vec<_>>()
            && !entries.is_empty()
        {
            let mut ok = true;
            for e in entries {
                let p = fs::canonicalize(e.path()).unwrap_or_else(|_| e.path());
                ok &= disks(&p, depth + 1, out);
            }
            return ok;
        }
        let whole = if sys.join("partition").exists() {
            sys.parent().map(Path::to_path_buf)
        } else {
            Some(sys.to_path_buf())
        };
        let Some(whole) = whole else { return false };
        match fs::read_to_string(whole.join("dev")) {
            Ok(d) => {
                out.insert(format!("linux:{}", d.trim()));
                true
            }
            Err(_) => false,
        }
    }

    fn by_dev(major: u64, minor: u64) -> Placement {
        let link = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
        let Ok(sys) = fs::canonicalize(&link) else {
            return Placement::Unknown(format!("no sysfs entry for {major}:{minor}"));
        };
        let mut out = BTreeSet::new();
        if disks(&sys, 0, &mut out) && !out.is_empty() {
            Placement::Local(out)
        } else {
            Placement::Unknown(format!("cannot resolve {}", sys.display()))
        }
    }

    pub fn placement_of_device(path: &str) -> Placement {
        match fs::metadata(path) {
            Ok(m) => {
                let (ma, mi) = split(m.rdev());
                by_dev(ma, mi)
            }
            Err(e) => unknown(&e),
        }
    }

    pub fn placement_of_path(path: &Path) -> Placement {
        let m = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) => return unknown(&e),
        };
        let (ma, mi) = split(m.dev());
        if ma != 0 {
            return by_dev(ma, mi);
        }
        // Anonymous device (btrfs, overlay, network, FUSE): consult the mount table.
        let Ok(info) = fs::read_to_string("/proc/self/mountinfo") else {
            return Placement::Unknown("no mountinfo".into());
        };
        let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut best: Option<(usize, String, String)> = None;
        for line in info.lines() {
            let fields: Vec<&str> = line.split(' ').collect();
            let Some(sep) = fields.iter().position(|f| *f == "-") else {
                continue;
            };
            let (Some(mnt), Some(fstype), Some(source)) =
                (fields.get(4), fields.get(sep + 1), fields.get(sep + 2))
            else {
                continue;
            };
            let mnt = mnt.replace("\\040", " ");
            if canon.starts_with(&mnt) && best.as_ref().is_none_or(|b| mnt.len() > b.0) {
                best = Some((mnt.len(), (*fstype).to_owned(), (*source).to_owned()));
            }
        }
        let Some((_, fstype, source)) = best else {
            return Placement::Unknown("no mount".into());
        };
        if matches!(
            fstype.as_str(),
            "nfs" | "nfs4" | "cifs" | "smb3" | "smbfs" | "9p" | "afs"
        ) {
            return Placement::Remote(fstype);
        }
        if source.starts_with("/dev/") {
            return placement_of_device(&source);
        }
        Placement::Unknown(format!("{fstype} on {source}"))
    }

    pub fn target_info(path: &Path) -> TargetInfo {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return TargetInfo::default();
        };
        // SAFETY: `statfs` is plain old data; all-zero is a valid value to be overwritten.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid NUL-terminated path and `st` is a properly sized out-parameter.
        let ok = unsafe { libc::statfs(c.as_ptr(), &mut st) } == 0;
        if !ok {
            return TargetInfo::default();
        }
        #[allow(
            clippy::unnecessary_cast,
            reason = "f_type width differs between Linux targets"
        )]
        let fs = match st.f_type as i64 {
            0x4d44 => "fat",
            0x2011_BAB0 => "exfat",
            0x5346_544e | 0x7366_746e => "ntfs",
            0xEF53 => "ext4",
            0x9123_683E => "btrfs",
            0x5846_5342 => "xfs",
            0x0102_1994 => "tmpfs",
            0x6573_5546 => "fuse",
            _ => "other",
        };
        #[allow(
            clippy::unnecessary_cast,
            clippy::cast_sign_loss,
            reason = "statfs field widths differ between Linux targets"
        )]
        let free = (st.f_bavail as u64).checked_mul(st.f_bsize as u64);
        TargetInfo {
            fs: Some(fs.into()),
            free_bytes: free,
        }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use super::{Placement, TargetInfo};
    use std::collections::BTreeSet;
    use std::ffi::CStr;
    use std::path::Path;
    use std::process::Command;

    fn statfs(path: &Path) -> Option<libc::statfs> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `statfs` is plain old data; all-zero is a valid value to be overwritten.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: valid NUL-terminated path and a properly sized out-parameter.
        (unsafe { libc::statfs(c.as_ptr(), &mut st) } == 0).then_some(st)
    }

    fn cstr(buf: &[libc::c_char]) -> String {
        // SAFETY: statfs fills these fixed arrays with NUL-terminated strings.
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    /// Whole disks behind a BSD device name, through APFS physical stores.
    fn whole_disks(dev: &str, depth: u32, out: &mut BTreeSet<String>) -> bool {
        if depth > 4 {
            return false;
        }
        let name = dev.trim_start_matches("/dev/").trim_start_matches('r');
        let Ok(o) = Command::new("/usr/sbin/diskutil")
            .args(["info", "-plist", name])
            .output()
        else {
            return false;
        };
        let text = String::from_utf8_lossy(&o.stdout);
        let value = |key: &str| -> Option<String> {
            let i = text.find(&format!("<key>{key}</key>"))?;
            let rest = text.get(i..)?;
            let a = rest.find("<string>")? + 8;
            let b = rest.find("</string>")?;
            rest.get(a..b).map(str::to_owned)
        };
        let stores: Vec<String> = text
            .split("<key>APFSPhysicalStores</key>")
            .nth(1)
            .map(|t| {
                t.split("</array>")
                    .next()
                    .unwrap_or("")
                    .split("<string>")
                    .skip(1)
                    .filter_map(|s| s.split("</string>").next().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if !stores.is_empty() {
            return stores.iter().all(|s| whole_disks(s, depth + 1, out));
        }
        match value("ParentWholeDisk") {
            Some(w) => {
                out.insert(format!("macos:{w}"));
                true
            }
            None => false,
        }
    }

    pub fn placement_of_device(path: &str) -> Placement {
        let mut out = BTreeSet::new();
        if whole_disks(path, 0, &mut out) && !out.is_empty() {
            Placement::Local(out)
        } else {
            Placement::Unknown(format!("cannot resolve {path}"))
        }
    }

    pub fn placement_of_path(path: &Path) -> Placement {
        let Some(st) = statfs(path) else {
            return Placement::Unknown("statfs failed".into());
        };
        let fstype = cstr(&st.f_fstypename);
        let from = cstr(&st.f_mntfromname);
        if matches!(fstype.as_str(), "smbfs" | "nfs" | "afpfs" | "webdav") {
            return Placement::Remote(fstype);
        }
        if from.starts_with("/dev/") {
            return placement_of_device(&from);
        }
        Placement::Unknown(format!("{fstype} on {from}"))
    }

    pub fn target_info(path: &Path) -> TargetInfo {
        let Some(st) = statfs(path) else {
            return TargetInfo::default();
        };
        let fs = cstr(&st.f_fstypename).to_lowercase();
        let free = st.f_bavail.checked_mul(u64::from(st.f_bsize));
        TargetInfo {
            fs: Some(fs),
            free_bytes: free,
        }
    }
}

#[cfg(windows)]
mod sys {
    use super::{Placement, TargetInfo};
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::fs::OpenOptions;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetVolumeInformationW,
        GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::IOCTL_STORAGE_GET_DEVICE_NUMBER;

    /// CTL_CODE(IOCTL_VOLUME_BASE, 0, METHOD_BUFFERED, FILE_ANY_ACCESS); not exported by windows-sys.
    const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    fn from_wide(b: &[u16]) -> String {
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        OsString::from_wide(b.get(..end).unwrap_or_default())
            .to_string_lossy()
            .into_owned()
    }

    fn volume_root(path: &Path) -> Option<String> {
        let w = wide(path);
        let mut buf = vec![0u16; 1024];
        // SAFETY: `w` is NUL-terminated; `buf` is valid for its stated length.
        let ok = unsafe {
            GetVolumePathNameW(
                w.as_ptr(),
                buf.as_mut_ptr(),
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
            )
        };
        (ok != 0).then(|| from_wide(&buf))
    }

    fn ioctl(path: &str, code: u32, out: &mut [u8]) -> Option<usize> {
        let f = OpenOptions::new()
            .access_mode(0)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
            .ok()?;
        let mut returned: u32 = 0;
        // SAFETY: valid handle for the lifetime of `f`; `out` is valid for its stated length.
        let ok = unsafe {
            DeviceIoControl(
                f.as_raw_handle() as _,
                code,
                std::ptr::null(),
                0,
                out.as_mut_ptr().cast(),
                u32::try_from(out.len()).ok()?,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then(|| usize::try_from(returned).unwrap_or(0))
    }

    fn u32_at(b: &[u8], o: usize) -> Option<u32> {
        b.get(o..o + 4)
            .and_then(|s| s.try_into().ok())
            .map(u32::from_le_bytes)
    }

    /// Disk numbers of a volume device path like `\\.\E:` or `\\?\Volume{…}`.
    fn volume_disks(volume: &str) -> Option<BTreeSet<String>> {
        let mut out = vec![0u8; 4096];
        let n = ioctl(volume, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &mut out)?;
        let out = out.get(..n)?;
        let count = usize::try_from(u32_at(out, 0)?).ok()?.min(64);
        // VOLUME_DISK_EXTENTS: u32 count, u32 pad, then 24-byte DISK_EXTENT { u32 disk, pad, i64 start, i64 len }.
        let set: BTreeSet<String> = (0..count)
            .filter_map(|i| u32_at(out, 8 + i * 24))
            .map(|d| format!("windows:PhysicalDrive{d}"))
            .collect();
        (!set.is_empty()).then_some(set)
    }

    pub fn placement_of_device(path: &str) -> Placement {
        let lower = path.to_ascii_lowercase();
        if let Some(n) = lower.strip_prefix(r"\\.\physicaldrive") {
            return Placement::Local(BTreeSet::from([format!("windows:PhysicalDrive{n}")]));
        }
        match volume_disks(path.trim_end_matches('\\')) {
            Some(s) => Placement::Local(s),
            None => {
                // STORAGE_DEVICE_NUMBER { u32 type, u32 number, u32 partition }
                let mut out = vec![0u8; 12];
                match ioctl(path, IOCTL_STORAGE_GET_DEVICE_NUMBER, &mut out).and_then(|_| u32_at(&out, 4)) {
                    Some(n) => Placement::Local(BTreeSet::from([format!("windows:PhysicalDrive{n}")])),
                    None => Placement::Unknown(format!("cannot resolve {path}")),
                }
            }
        }
    }

    pub fn placement_of_path(path: &Path) -> Placement {
        let s = path.to_string_lossy();
        if s.starts_with(r"\\") && !s.starts_with(r"\\?\") && !s.starts_with(r"\\.\") {
            return Placement::Remote("unc".into());
        }
        let Some(root) = volume_root(path) else {
            return Placement::Unknown("no volume root".into());
        };
        let w: Vec<u16> = std::ffi::OsStr::new(&root)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut name = vec![0u16; 64];
        // SAFETY: `w` is NUL-terminated; `name` is valid for its stated length.
        let ok = unsafe {
            GetVolumeNameForVolumeMountPointW(
                w.as_ptr(),
                name.as_mut_ptr(),
                u32::try_from(name.len()).unwrap_or(u32::MAX),
            )
        };
        if ok == 0 {
            return Placement::Remote("not a local volume".into());
        }
        let vol = from_wide(&name);
        match volume_disks(vol.trim_end_matches('\\')) {
            Some(s) => Placement::Local(s),
            None => Placement::Unknown(format!("cannot resolve {vol}")),
        }
    }

    pub fn target_info(path: &Path) -> TargetInfo {
        let Some(root) = volume_root(path) else {
            return TargetInfo::default();
        };
        let w: Vec<u16> = std::ffi::OsStr::new(&root)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut fsname = vec![0u16; 64];
        // SAFETY: `w` is NUL-terminated; out buffers are valid for their stated lengths or null.
        let ok = unsafe {
            GetVolumeInformationW(
                w.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fsname.as_mut_ptr(),
                u32::try_from(fsname.len()).unwrap_or(u32::MAX),
            )
        };
        let fs = (ok != 0).then(|| from_wide(&fsname).to_lowercase());
        let mut free: u64 = 0;
        // SAFETY: `w` is NUL-terminated; `free` is a valid out-parameter; others may be null.
        let ok =
            unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
        TargetInfo {
            fs,
            free_bytes: (ok != 0).then_some(free),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod sys {
    use super::{Placement, TargetInfo};
    use std::path::Path;

    pub fn placement_of_device(_: &str) -> Placement {
        Placement::Unknown("unsupported platform".into())
    }

    pub fn placement_of_path(_: &Path) -> Placement {
        Placement::Unknown("unsupported platform".into())
    }

    pub fn target_info(_: &Path) -> TargetInfo {
        TargetInfo::default()
    }
}
