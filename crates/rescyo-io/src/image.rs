//! Raw image files as Sources.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::{BlockSource, Geometry, ReadError, SourceIdentity, SourceKind};

/// A raw (`dd`-style) image file opened for reading only.
#[derive(Debug)]
pub struct ImageSource {
    file: File,
    geometry: Geometry,
    identity: SourceIdentity,
}

impl ImageSource {
    /// Opens `path` read-only. `logical_sector` is usually 512; 4096 for 4Kn images.
    pub fn open(path: &Path, logical_sector: u32) -> io::Result<Self> {
        if !logical_sector.is_power_of_two() || !(512..=65536).contains(&logical_sector) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sector size must be 512..=65536",
            ));
        }
        let file = File::open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
        }
        // A trailing partial sector cannot be addressed through sector-aligned reads.
        let size = meta.len() - meta.len() % u64::from(logical_sector);
        let geometry = Geometry { size, logical_sector };
        let identity = SourceIdentity {
            kind: SourceKind::Image,
            path: path.display().to_string(),
            model: None,
            serial: None,
        };
        Ok(Self {
            file,
            geometry,
            identity,
        })
    }
}

impl BlockSource for ImageSource {
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

/// Positional read without moving a shared cursor, so `&self` reads are thread-safe.
#[cfg(unix)]
pub(crate) fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(windows)]
pub(crate) fn read_exact_at(file: &File, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = buf.get_mut(n..).unwrap_or_default();
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Maps OS errors to media errors (worth zero-filling) versus a vanished device.
pub(crate) fn classify(e: io::Error) -> ReadError {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        return ReadError::OutOfBounds;
    }
    match e.raw_os_error() {
        Some(code) if is_media_error(code) => ReadError::Media,
        Some(code) if is_gone_error(code) => ReadError::Gone(e),
        _ => ReadError::Io(e),
    }
}

#[cfg(unix)]
fn is_media_error(code: i32) -> bool {
    // EIO, ENODATA (Linux: medium error on some drivers), EBADMSG (CRC on some stacks).
    const EIO: i32 = 5;
    #[cfg(target_os = "linux")]
    const EXTRA: &[i32] = &[61, 74];
    #[cfg(not(target_os = "linux"))]
    const EXTRA: &[i32] = &[];
    code == EIO || EXTRA.contains(&code)
}

#[cfg(unix)]
fn is_gone_error(code: i32) -> bool {
    // ENXIO, ENODEV
    #[cfg(target_os = "linux")]
    const GONE: &[i32] = &[6, 19];
    #[cfg(not(target_os = "linux"))]
    const GONE: &[i32] = &[6, 19];
    GONE.contains(&code)
}

#[cfg(windows)]
fn is_media_error(code: i32) -> bool {
    // ERROR_CRC, ERROR_SECTOR_NOT_FOUND, ERROR_READ_FAULT, ERROR_IO_DEVICE, ERROR_DISK_OPERATION_FAILED
    matches!(code, 23 | 27 | 30 | 1117 | 1127)
}

#[cfg(windows)]
fn is_gone_error(code: i32) -> bool {
    // ERROR_NOT_READY, ERROR_DEV_NOT_EXIST, ERROR_DEVICE_NOT_CONNECTED, ERROR_NO_SUCH_DEVICE
    matches!(code, 21 | 55 | 1167 | 433)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::pattern;
    use std::io::Write;

    fn image(len: usize) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&pattern(len)).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn reads_exact_bytes_at_offset() {
        let f = image(8192);
        let src = ImageSource::open(f.path(), 512).unwrap();
        assert_eq!(
            src.geometry(),
            Geometry {
                size: 8192,
                logical_sector: 512
            }
        );
        let mut buf = vec![0; 1024];
        src.read_at(1024, &mut buf).unwrap();
        assert_eq!(buf, pattern(8192)[1024..2048]);
    }

    #[test]
    fn rejects_out_of_bounds_and_misaligned_reads() {
        let f = image(4096);
        let src = ImageSource::open(f.path(), 512).unwrap();
        let mut buf = vec![0; 1024];
        assert!(matches!(src.read_at(3584, &mut buf), Err(ReadError::OutOfBounds)));
        assert!(matches!(src.read_at(100, &mut buf), Err(ReadError::Misaligned)));
        let mut odd = vec![0; 100];
        assert!(matches!(src.read_at(0, &mut odd), Err(ReadError::Misaligned)));
    }

    #[test]
    fn rejects_bad_sector_sizes_and_directories() {
        let f = image(4096);
        assert!(ImageSource::open(f.path(), 1000).is_err());
        assert!(ImageSource::open(f.path(), 256).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(ImageSource::open(dir.path(), 512).is_err());
    }
}
