//! Windows helpers: user SID lookup for the pipe name, the user-only pipe DACL, and the
//! peer SID check (01 §1.2).

use std::io;

use sha2::Digest;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

const SDDL_REVISION_1: u32 = 1;

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own this handle.
        unsafe { CloseHandle(self.0) };
    }
}

/// String SID (`S-1-5-21-…`) of the user owning `process`.
fn process_sid_string(process: HANDLE) -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: valid process handle and out-pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    let mut len = 0u32;
    // SAFETY: size query with a null buffer is the documented pattern.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 storage keeps TOKEN_USER suitably aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buffer is at least `len` bytes.
    if unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the API filled a TOKEN_USER at the start of `buf`.
    let user = unsafe { &*(buf.as_ptr().cast::<TOKEN_USER>()) };
    let mut wide: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid SID pointer from the token; out-pointer receives a LocalAlloc'd string.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut wide) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `wide` is a NUL-terminated UTF-16 string.
    let s = unsafe {
        let mut n = 0;
        while *wide.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(wide, n))
    };
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc.
    unsafe { LocalFree(wide.cast()) };
    Ok(s)
}

pub fn current_user_sid() -> io::Result<String> {
    // SAFETY: pseudo-handle, never needs closing.
    process_sid_string(unsafe { GetCurrentProcess() })
}

/// First 16 hex chars of SHA-256 of the user SID, used in the pipe name.
pub fn current_user_sid_hash() -> io::Result<String> {
    let sid = current_user_sid()?;
    let h = sha2::Sha256::digest(sid.as_bytes());
    Ok(hex::encode(h)[..16].to_string())
}

/// SID of the user running process `pid` (peer check on accept).
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    // SAFETY: plain API call; a null return is checked.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    let h = OwnedHandle(h);
    process_sid_string(h.0)
}

/// `SECURITY_ATTRIBUTES` with a protected DACL granting GENERIC_ALL to the current user
/// SID only. (The owner alias `OW` is avoided: for elevated admins the owner is the
/// Administrators group.)
pub struct UserOnlySecurity {
    sa: SECURITY_ATTRIBUTES,
    sd: PSECURITY_DESCRIPTOR,
}

// SAFETY: the descriptor is immutable heap memory owned by this value.
unsafe impl Send for UserOnlySecurity {}
unsafe impl Sync for UserOnlySecurity {}

impl UserOnlySecurity {
    pub fn new() -> io::Result<Self> {
        let sid = current_user_sid()?;
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})\0").encode_utf16().collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: NUL-terminated input; out-pointer receives a LocalAlloc'd descriptor.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        Ok(UserOnlySecurity { sa, sd })
    }

    /// Pointer for `ServerOptions::create_with_security_attributes_raw`.
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        (&raw const self.sa).cast_mut().cast()
    }
}

impl Drop for UserOnlySecurity {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.sd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_lookups_agree() {
        let me = current_user_sid().unwrap();
        assert!(me.starts_with("S-1-"));
        assert_eq!(process_user_sid(std::process::id()).unwrap(), me);
        assert_eq!(current_user_sid_hash().unwrap().len(), 16);
        let _sa = UserOnlySecurity::new().unwrap();
    }
}
