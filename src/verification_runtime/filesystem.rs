//! Preserve the verification-specific refusal while sharing the FD capability check.
use super::{Error, unverified};
use std::fs::File;
pub(super) fn require_local(file: &File) -> Result<(), Error> {
    if !crate::local_filesystem::supported_file(file)? {
        return Err(unverified(
            "filesystem is outside the local runtime-guard contract",
        ));
    }
    Ok(())
}
