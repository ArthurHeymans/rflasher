//! Cooperative userspace exclusion and kernel-driver exclusion before MMIO.

use super::AtiSpiError;
use std::fs::{self, File};
use std::path::Path;

/// The open config file holds an advisory lock until after controller restore.
/// Users must set driver_override to "none" before unbinding the GPU, preventing
/// automatic kernel-driver binding for the entire flashing session.
pub(super) struct DeviceClaim {
    _lock: File,
}

impl DeviceClaim {
    pub(super) fn acquire(device_path: &Path) -> Result<Self, AtiSpiError> {
        let lock = File::open(device_path.join("config"))
            .map_err(|e| AtiSpiError::Ownership(format!("cannot open PCI config: {e}")))?;
        lock.try_lock().map_err(|e| {
            AtiSpiError::Ownership(format!("cannot lock PCI device (already in use?): {e}"))
        })?;

        match fs::symlink_metadata(device_path.join("driver")) {
            Ok(_) => {
                return Err(AtiSpiError::Ownership(
                    "GPU has a bound kernel driver; set driver_override to 'none', then unbind the GPU before using ati_spi".into(),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(AtiSpiError::Ownership(format!(
                    "cannot check GPU driver binding: {e}"
                )));
            }
        }
        let driver_override = fs::read_to_string(device_path.join("driver_override"))
            .map_err(|e| AtiSpiError::Ownership(format!("cannot read driver_override: {e}")))?;
        if driver_override.trim() != "none" {
            return Err(AtiSpiError::Ownership(
                "set the GPU's driver_override to 'none' before unbinding it, and leave it set until ati_spi exits".into(),
            ));
        }
        Ok(Self { _lock: lock })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn requires_unbound_device_and_disabled_driver_matching() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fs::write(path.join("config"), []).unwrap();
        // Missing/unreadable ownership metadata must fail closed.
        assert!(DeviceClaim::acquire(path).is_err());
        for value in ["(null)\n", "\n", "amdgpu\n"] {
            fs::write(path.join("driver_override"), value).unwrap();
            assert!(DeviceClaim::acquire(path).is_err());
        }
        fs::write(path.join("driver_override"), "none\n").unwrap();
        // Even a dangling driver symlink must cause refusal.
        symlink("/nonexistent/amdgpu", path.join("driver")).unwrap();
        assert!(DeviceClaim::acquire(path).is_err());
        fs::remove_file(path.join("driver")).unwrap();
        assert!(DeviceClaim::acquire(path).is_ok());
    }

    #[test]
    fn lock_excludes_other_opens_and_is_released_on_drop_or_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fs::write(path.join("config"), []).unwrap();
        fs::write(path.join("driver_override"), "none\n").unwrap();
        let first = DeviceClaim::acquire(path).unwrap();
        assert!(DeviceClaim::acquire(path).is_err());
        drop(first);
        assert!(DeviceClaim::acquire(path).is_ok());
        fs::write(path.join("driver_override"), "amdgpu\n").unwrap();
        assert!(DeviceClaim::acquire(path).is_err());
        fs::write(path.join("driver_override"), "none\n").unwrap();
        assert!(DeviceClaim::acquire(path).is_ok());
    }
}
