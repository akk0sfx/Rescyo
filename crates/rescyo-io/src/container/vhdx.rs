//! Placeholder: implemented in a following commit.

use std::path::Path;

use super::{ContainerFormat, OpenError, Opened};

pub(super) fn open(_path: &Path) -> Result<Opened, OpenError> {
    Err(OpenError::unsupported(ContainerFormat::Vhdx, "this format"))
}
