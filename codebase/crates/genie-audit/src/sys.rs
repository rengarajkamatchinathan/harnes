//! Small OS probes for `genie doctor` (01 §8): free disk space.
//!
//! `unsafe` is FFI only (`statvfs`, `GetDiskFreeSpaceExW`), each with a `// SAFETY:` note.

use std::io;
use std::path::Path;

/// Bytes available to this user on the volume holding `path`.
pub fn disk_free(path: &Path) -> io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `c` is NUL-terminated; `st` is a valid out pointer.
        let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: statvfs returned 0, so `st` is initialised.
        let st = unsafe { st.assume_init() };
        #[allow(clippy::unnecessary_cast)] // field widths differ per platform
        Ok(st.f_bavail as u64 * st.f_frsize as u64)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let w: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut free = 0u64;
        // SAFETY: `w` is NUL-terminated; out pointers are valid or null.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                w.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(free)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "not supported"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn temp_dir_has_space() {
        let free = super::disk_free(&std::env::temp_dir()).unwrap();
        assert!(free > 0);
        assert!(super::disk_free(std::path::Path::new("/definitely/not/here")).is_err());
    }
}
