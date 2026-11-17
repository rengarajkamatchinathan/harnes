//! Windows platform access for managed policy (08 §5.1): the HKLM registry reader and the
//! `policy.json` ACL query. The decisions themselves are in [`crate::mdm`] and tested on
//! every OS; this module only turns Win32 data into those inputs.
//!
//! `unsafe` here is Win32 FFI only; every block has a `// SAFETY:` note (08 §9).

use std::ffi::c_void;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ,
    REG_QWORD, REG_SZ, RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
};

use crate::mdm::{AceInfo, RegKey, RegValue, RegistryReader, acl_admin_only};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const INHERIT_ONLY_ACE: u8 = 0x08;
const ERROR_FILE_NOT_FOUND: u32 = 2;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// `HKEY_LOCAL_MACHINE` (64-bit view).
pub struct WinRegistry;

struct OpenKey(HKEY);

impl Drop for OpenKey {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a key handle opened by `RegOpenKeyExW` and closed once here.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn decode(kind: u32, data: &[u8]) -> Option<RegValue> {
    let words = || -> Vec<u16> {
        data.as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect()
    };
    match kind {
        REG_DWORD if data.len() >= 4 => Some(RegValue::Dword(u32::from_le_bytes([
            data[0], data[1], data[2], data[3],
        ]))),
        REG_QWORD if data.len() >= 8 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[..8]);
            Some(RegValue::Qword(u64::from_le_bytes(b)))
        }
        REG_SZ | REG_EXPAND_SZ => Some(RegValue::Sz(from_wide(&words()))),
        REG_MULTI_SZ => Some(RegValue::MultiSz(
            words()
                .split(|c| *c == 0)
                .filter(|s| !s.is_empty())
                .map(String::from_utf16_lossy)
                .collect(),
        )),
        _ => None,
    }
}

impl RegistryReader for WinRegistry {
    fn read_key(&self, path: &str) -> Result<Option<RegKey>, String> {
        let wpath = wide(path);
        let mut hkey: HKEY = null_mut();
        // SAFETY: `wpath` is NUL-terminated and outlives the call; `hkey` is a valid out
        // pointer.
        let rc = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                wpath.as_ptr(),
                0,
                KEY_READ | KEY_WOW64_64KEY,
                &mut hkey,
            )
        };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if rc != ERROR_SUCCESS {
            return Err(format!("RegOpenKeyExW failed ({rc})"));
        }
        let key = OpenKey(hkey);
        let mut out = RegKey::default();
        let mut name = vec![0u16; 16_384];
        let mut data = vec![0u8; 64 * 1024];
        for i in 0u32.. {
            let mut name_len = name.len() as u32;
            let mut data_len = data.len() as u32;
            let mut kind = 0u32;
            // SAFETY: buffers and their lengths match; the key handle is open.
            let rc = unsafe {
                RegEnumValueW(
                    key.0,
                    i,
                    name.as_mut_ptr(),
                    &mut name_len,
                    null(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut data_len,
                )
            };
            if rc == ERROR_NO_MORE_ITEMS {
                break;
            }
            if rc != ERROR_SUCCESS {
                // Too large or unreadable: skip this value rather than drop the policy.
                continue;
            }
            let n = String::from_utf16_lossy(&name[..name_len as usize]);
            if let Some(v) = decode(kind, &data[..data_len as usize]) {
                out.values.push((n, v));
            }
        }
        for i in 0u32.. {
            let mut name_len = name.len() as u32;
            // SAFETY: as above; optional class/time out-pointers are null.
            let rc = unsafe {
                RegEnumKeyExW(
                    key.0,
                    i,
                    name.as_mut_ptr(),
                    &mut name_len,
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                )
            };
            if rc != ERROR_SUCCESS {
                break;
            }
            out.subkeys
                .push(String::from_utf16_lossy(&name[..name_len as usize]));
        }
        Ok(Some(out))
    }
}

fn sid_string(sid: PSID) -> Option<String> {
    let mut s: *mut u16 = null_mut();
    // SAFETY: `sid` points into a security descriptor we hold; `s` receives a LocalAlloc'd
    // string that we free below.
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut s) };
    if ok == 0 || s.is_null() {
        return None;
    }
    // SAFETY: `s` is a NUL-terminated wide string from the call above.
    let out = unsafe {
        let mut len = 0usize;
        while *s.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(s, len))
    };
    // SAFETY: allocated by `ConvertSidToStringSidW` with LocalAlloc.
    unsafe {
        LocalFree(s as *mut c_void);
    }
    Some(out)
}

/// The owner and DACL of `path` → [`acl_admin_only`].
pub fn admin_only(path: &Path) -> std::io::Result<bool> {
    std::fs::metadata(path)?;
    let wpath = wide(&path.to_string_lossy());
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: valid NUL-terminated path; out pointers are valid; `sd` is freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(rc as i32));
    }
    let owner_sid = sid_string(owner).unwrap_or_default();
    let aces = if dacl.is_null() {
        None
    } else {
        let mut list = Vec::new();
        // SAFETY: `dacl` points into `sd`, which stays alive until LocalFree below.
        let count = unsafe { (*dacl).AceCount };
        for i in 0..u32::from(count) {
            let mut ace: *mut c_void = null_mut();
            // SAFETY: index < AceCount; `ace` receives a pointer into the ACL.
            if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
                continue;
            }
            // SAFETY: every ACE starts with an ACE_HEADER.
            let header = unsafe { *(ace as *const ACE_HEADER) };
            let allow = header.AceType == ACCESS_ALLOWED_ACE_TYPE;
            let (mask, sid) = if allow {
                let a = ace as *const ACCESS_ALLOWED_ACE;
                // SAFETY: AceType says this is an ACCESS_ALLOWED_ACE; its SID starts at
                // `SidStart`.
                unsafe {
                    (
                        (*a).Mask,
                        sid_string(&(*a).SidStart as *const u32 as PSID).unwrap_or_default(),
                    )
                }
            } else {
                (0, String::new())
            };
            list.push(AceInfo {
                allow,
                inherit_only: header.AceFlags & INHERIT_ONLY_ACE != 0,
                mask,
                sid,
            });
        }
        Some(list)
    };
    // SAFETY: `sd` was allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(sd);
    }
    Ok(acl_admin_only(&owner_sid, aces.as_deref()))
}
