//! Placeholder: implemented in a following commit.

use std::path::Path;

use super::{ContainerFormat, OpenError, Opened};

pub(super) fn open(_path: &Path) -> Result<Opened, OpenError> {
    Err(OpenError::unsupported(ContainerFormat::Vmdk, "this format"))
}

/// Whether `head` is a VMDK text descriptor.
pub(super) fn looks_like_descriptor(head: &[u8]) -> bool {
    head.starts_with(b"# Disk DescriptorFile")
}
