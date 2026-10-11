//! Placeholder: implemented in a following commit.

use std::path::Path;

use super::{ContainerFormat, OpenError, Opened};

/// Segment-file signature of EnCase (E01) evidence files.
pub(super) const SIGNATURE: &[u8] = b"EVF\x09\x0d\x0a\xff\x00";
/// Segment-file signature of SMART (S01) evidence files.
pub(super) const SIGNATURE_SMART: &[u8] = b"LVF\x09\x0d\x0a\xff\x00";

pub(super) fn open(_path: &Path) -> Result<Opened, OpenError> {
    Err(OpenError::unsupported(ContainerFormat::Ewf, "this format"))
}
