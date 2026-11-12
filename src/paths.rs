//! Folders, renames and deletes, with the errors std looks for.
//!
//! rootshell's file broker makes folders, renames and deletes with
//! FileManager, and answers EIO for whatever stops one (its
//! rootshell/Features/Wasm/WasmFilesystemBroker.swift): a folder that's
//! there already, or one whose parent isn't; a rename onto a file that's
//! there, which POSIX replaces; a delete of what isn't there. std goes by
//! EEXIST and ENOENT: `fs::create_dir_all` stops at the first other error,
//! so this program's second run stopped at its own folder, and signing
//! would stop at the first folder it makes twice. So this program has its
//! own versions of the calls wasi-libc's `mkdir`, `rename`, `unlink` and
//! `rmdir` end in, and the `*at` ones too: when one fails, they look at
//! what's there and answer as POSIX does, and a rename onto a file deletes
//! the file first.

#[cfg(target_os = "wasi")]
mod wasi {
    use std::ffi::{CStr, CString, c_char, c_int};
    use std::mem::MaybeUninit;

    /// WASI's `filestat`.
    #[repr(C)]
    #[allow(dead_code, reason = "WASI's layout, of which only the type's read")]
    struct Filestat {
        dev: u64,
        ino: u64,
        filetype: u8,
        nlink: u64,
        size: u64,
        atim: u64,
        mtim: u64,
        ctim: u64,
    }

    unsafe extern "C" {
        /// wasi-libc's calls of WASI's own.
        fn __wasi_path_create_directory(fd: u32, path: *const c_char) -> u16;
        fn __wasi_path_unlink_file(fd: u32, path: *const c_char) -> u16;
        fn __wasi_path_remove_directory(fd: u32, path: *const c_char) -> u16;
        fn __wasi_path_rename(fd: u32, path: *const c_char, to_fd: u32, to: *const c_char) -> u16;
        fn __wasi_path_filestat_get(
            fd: u32,
            flags: u32,
            path: *const c_char,
            stat: *mut Filestat,
        ) -> u16;
        fn __errno_location() -> *mut c_int;
    }

    const EEXIST: u16 = 20;
    const ENOENT: u16 = 44;
    const DIRECTORY: u8 = 3;

    /// The type of what's at `path` in the folder `fd`, as WASI has it,
    /// without following a link; or why nothing's there.
    fn look(fd: c_int, path: &CStr) -> Result<u8, u16> {
        let mut stat = MaybeUninit::<Filestat>::uninit();
        match unsafe { __wasi_path_filestat_get(fd as u32, 0, path.as_ptr(), stat.as_mut_ptr()) } {
            0 => Ok(unsafe { stat.assume_init() }.filetype),
            error => Err(error),
        }
    }

    /// The folder `path` is in, when it's not `fd` itself.
    fn parent(path: &CStr) -> Option<CString> {
        let bytes = path.to_bytes();
        let end = bytes.iter().rposition(|&byte| byte != b'/')? + 1;
        let slash = bytes[..end].iter().rposition(|&byte| byte == b'/')?;
        CString::new(&bytes[..slash.max(1)]).ok()
    }

    /// Ends a call as wasi-libc's do.
    fn answer(error: u16) -> c_int {
        if error == 0 {
            return 0;
        }
        unsafe { *__errno_location() = c_int::from(error) };
        -1
    }

    /// `error` for a call on `path`, or ENOENT when nothing's there.
    fn or_missing(fd: c_int, path: &CStr, error: u16) -> u16 {
        match look(fd, path) {
            Err(ENOENT) => ENOENT,
            _ => error,
        }
    }

    /// Makes a folder, in place of wasi-libc's: under `mkdir`, `mkdirat`.
    ///
    /// # Safety
    ///
    /// `path` is a C string.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wasilibc_nocwd_mkdirat_nomode(
        fd: c_int,
        path: *const c_char,
    ) -> c_int {
        let error = unsafe { __wasi_path_create_directory(fd as u32, path) };
        if error == 0 {
            return 0;
        }
        let path = unsafe { CStr::from_ptr(path) };
        answer(if look(fd, path).is_ok() {
            EEXIST
        } else {
            match parent(path) {
                Some(parent) => or_missing(fd, &parent, error),
                None => error,
            }
        })
    }

    /// Deletes a file, in place of wasi-libc's: under `unlink`, `unlinkat`.
    ///
    /// # Safety
    ///
    /// `path` is a C string.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wasilibc_nocwd___wasilibc_unlinkat(
        fd: c_int,
        path: *const c_char,
    ) -> c_int {
        match unsafe { __wasi_path_unlink_file(fd as u32, path) } {
            0 => 0,
            error => answer(or_missing(fd, unsafe { CStr::from_ptr(path) }, error)),
        }
    }

    /// Deletes a folder, in place of wasi-libc's: under `rmdir`, `unlinkat`.
    ///
    /// # Safety
    ///
    /// `path` is a C string.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wasilibc_nocwd___wasilibc_rmdirat(
        fd: c_int,
        path: *const c_char,
    ) -> c_int {
        match unsafe { __wasi_path_remove_directory(fd as u32, path) } {
            0 => 0,
            error => answer(or_missing(fd, unsafe { CStr::from_ptr(path) }, error)),
        }
    }

    /// Renames, in place of wasi-libc's: under `rename`, `renameat`.
    ///
    /// # Safety
    ///
    /// `path` and `to` are C strings.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wasilibc_nocwd_renameat(
        fd: c_int,
        path: *const c_char,
        to_fd: c_int,
        to: *const c_char,
    ) -> c_int {
        let rename = || unsafe { __wasi_path_rename(fd as u32, path, to_fd as u32, to) };
        let error = rename();
        if error == 0 {
            return 0;
        }
        let (from, onto) = unsafe { (CStr::from_ptr(path), CStr::from_ptr(to)) };
        answer(match (look(fd, from), look(to_fd, onto)) {
            (Err(ENOENT), _) => ENOENT,
            // A file onto a file, which POSIX replaces and FileManager
            // doesn't.
            (Ok(kind), Ok(there)) if kind != DIRECTORY && there != DIRECTORY => {
                match unsafe { __wasi_path_unlink_file(to_fd as u32, to) } {
                    0 => rename(),
                    _ => error,
                }
            }
            _ => error,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn finds_the_folder_a_path_is_in() {
            let parent = |path: &str| {
                parent(&CString::new(path).unwrap()).map(|parent| parent.into_string().unwrap())
            };
            assert_eq!(parent("a/b/c").as_deref(), Some("a/b"));
            assert_eq!(parent("a/b/").as_deref(), Some("a"));
            assert_eq!(parent("a").as_deref(), None);
            assert_eq!(parent("/a").as_deref(), Some("/"));
            assert_eq!(parent("").as_deref(), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::ErrorKind;

    /// Under rootshell.py, which fails these as rootshell does, they only
    /// pass with this module's calls.
    #[test]
    fn folders_renames_and_deletes_fail_as_std_expects() {
        let folder = crate::testing::scratch("paths");
        let deep = folder.join("a/b/c");
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir_all(folder.join("a/b/d")).unwrap();
        assert_eq!(
            fs::create_dir(&deep).unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(
            fs::create_dir(folder.join("x/y")).unwrap_err().kind(),
            ErrorKind::NotFound
        );

        let (old, new) = (folder.join("old"), folder.join("new"));
        fs::write(&old, "old").unwrap();
        fs::write(&new, "new").unwrap();
        fs::rename(&old, &new).unwrap();
        assert_eq!(fs::read_to_string(&new).unwrap(), "old");
        assert!(!old.exists());
        assert_eq!(
            fs::rename(&old, &new).unwrap_err().kind(),
            ErrorKind::NotFound
        );

        assert_eq!(
            fs::remove_file(&old).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            fs::remove_dir(folder.join("x")).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        fs::remove_dir(folder.join("a/b/d")).unwrap();
        fs::remove_dir_all(&folder).unwrap();
        assert!(!folder.exists());
    }
}
