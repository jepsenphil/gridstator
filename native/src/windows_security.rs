//! Same-account access for Windows worker state and local named pipes.
use std::{ffi::c_void, io, os::windows::ffi::OsStrExt, path::Path, ptr};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
        },
        DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
        GetTokenInformation, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::{
        Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn check(success: i32) -> io::Result<()> {
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn forbidden(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

unsafe fn sid_string(sid: *mut c_void) -> io::Result<String> {
    let mut text = ptr::null_mut();
    check(unsafe { ConvertSidToStringSidW(sid, &mut text) })?;
    let allocated = Local(text.cast());
    let mut len = 0;
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    let value = String::from_utf16(unsafe { std::slice::from_raw_parts(text, len) })
        .map_err(|_| io::Error::other("invalid account SID"))?;
    drop(allocated);
    Ok(value)
}

fn process_sid(process: HANDLE) -> io::Result<String> {
    let mut token = ptr::null_mut();
    check(unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) })?;
    let token = Handle(token);
    let mut size = 0;
    unsafe {
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut size);
    }
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    // usize storage supplies TOKEN_USER's alignment, unlike a byte vector.
    let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    check(unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    })?;
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    unsafe { sid_string(user.User.Sid) }
}

pub(crate) fn current_sid() -> io::Result<String> {
    process_sid(unsafe { GetCurrentProcess() })
}

fn descriptor() -> io::Result<Local> {
    let sid = current_sid()?;
    // A protected DACL grants only this account access; new children inherit it.
    let sddl: Vec<u16> = format!("O:{sid}D:P(A;OICI;GA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = ptr::null_mut();
    check(unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    })?;
    Ok(Local(descriptor))
}

pub(crate) fn protect_path(path: &Path) -> io::Result<()> {
    let descriptor = descriptor()?;
    let mut acl = ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    check(unsafe {
        GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted)
    })?;
    let mut owner = ptr::null_mut();
    check(unsafe { GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted) })?;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let error = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            ptr::null_mut(),
            acl,
            ptr::null_mut(),
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    Ok(())
}

pub(crate) fn owned_path(path: &Path, private: bool) -> io::Result<()> {
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut owner = ptr::null_mut();
    let mut acl: *mut ACL = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    let error = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let _allocated = Local(descriptor);
    let sid = current_sid()?;
    if owner.is_null() || unsafe { sid_string(owner) }? != sid {
        return Err(forbidden(
            "FORBIDDEN: this account does not own worker state",
        ));
    }
    if private {
        if acl.is_null() {
            return Err(forbidden("FORBIDDEN: worker state has no private ACL"));
        }
        for index in 0..unsafe { (*acl).AceCount } as u32 {
            let mut ace = ptr::null_mut();
            check(unsafe { GetAce(acl, index, &mut ace) })?;
            let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            // Only ordinary allow ACEs for the owner, SYSTEM, or administrators are accepted.
            // SYSTEM and administrators can already bypass a private filesystem ACL.
            if ace.Header.AceType != 0 {
                return Err(forbidden("FORBIDDEN: unsupported worker state ACL"));
            }
            let trustee = unsafe { sid_string((&raw const ace.SidStart).cast_mut().cast()) }?;
            if trustee != sid && trustee != "S-1-5-18" && trustee != "S-1-5-32-544" {
                return Err(forbidden(
                    "FORBIDDEN: worker state must be private to this account",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn create_pipe(path: &Path, first: bool) -> io::Result<NamedPipeServer> {
    let descriptor = descriptor()?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // The descriptor and attributes remain alive throughout CreateNamedPipe.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(path, (&raw mut attributes).cast())
    }
}

pub(crate) fn verify_pipe_peer(handle: HANDLE, server: bool) -> io::Result<()> {
    let mut pid = 0;
    check(unsafe {
        if server {
            GetNamedPipeClientProcessId(handle, &mut pid)
        } else {
            GetNamedPipeServerProcessId(handle, &mut pid)
        }
    })?;
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = Handle(process);
    if process_sid(process.0)? != current_sid()? {
        return Err(forbidden("FORBIDDEN: worker control peer account mismatch"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_state_acl_and_owner_belong_to_this_account() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        protect_path(dir.path())?;
        owned_path(dir.path(), true)?;
        let file = dir.path().join("credential.json");
        std::fs::write(&file, b"private")?;
        // Elevated tokens can default new file ownership to Administrators. Private writers
        // explicitly assign the current account as owner as well as applying the inherited DACL.
        protect_path(&file)?;
        owned_path(&file, true)?;
        Ok(())
    }
}
