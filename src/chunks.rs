//! File reads and writes, in pieces rootshell can carry.
//!
//! rootshell hands each read's bytes back to the program through one 64 KB
//! slot of shared memory, as base64 in JSON, and a read that brings more
//! than fits fails with EINVAL, its bytes gone (its
//! Resources/Wasm/wasm-host.js, `writeReplyAndNotify`). Each write's bytes
//! go to rootshell's app as base64 too, which its Resources/Wasm/
//! wasm-worker.js builds a character at a time (`bytesToB64`): the signed
//! app, 38 MB, would be one string built 38 million times over. std reads a
//! file into a buffer as big as the file (`std::fs::read`), and writes one
//! out from one (`std::fs::write`), and so do the crates this program signs
//! and installs with. So this program has its own `read`, `readv`, `write`
//! and `writev`, which std's calls and wasi-libc's go to in place of
//! wasi-libc's: each reads [`MOST_READ`] at most, and writes
//! [`MOST_WRITTEN`] at most, as a short read or write, which every caller
//! takes. Receives from a connection are capped the same way in
//! `net.rs`.

/// The most one read asks for: its base64, 43,692 bytes, and the JSON
/// around it fit the 64 KB slot with room to spare.
#[allow(dead_code, reason = "rootshell's, and the tests'")]
pub const MOST_READ: usize = 32 * 1024;

/// The most one write hands over.
#[allow(dead_code, reason = "rootshell's, and the tests'")]
pub const MOST_WRITTEN: usize = 256 * 1024;

#[cfg(target_os = "wasi")]
mod wasi {
    use std::ffi::c_int;

    use super::{MOST_READ, MOST_WRITTEN};

    /// WASI's `iovec` and `ciovec`.
    #[repr(C)]
    pub struct Iovec {
        buf: *mut u8,
        len: usize,
    }

    unsafe extern "C" {
        /// wasi-libc's calls of WASI's own.
        fn __wasi_fd_read(fd: u32, iovs: *const Iovec, iovs_len: usize, read: *mut usize) -> u16;
        fn __wasi_fd_write(
            fd: u32,
            iovs: *const Iovec,
            iovs_len: usize,
            written: *mut usize,
        ) -> u16;
        fn __errno_location() -> *mut c_int;
    }

    /// WASI's "not capable", which wasi-libc reports as EBADF, as here.
    const ENOTCAPABLE: u16 = 76;
    const EBADF: c_int = 8;
    const EINVAL: c_int = 28;

    /// Ends a call as wasi-libc's `read` and `write` do.
    fn answer(error: u16, count: usize) -> isize {
        if error != 0 {
            let errno = if error == ENOTCAPABLE {
                EBADF
            } else {
                c_int::from(error)
            };
            unsafe { *__errno_location() = errno };
            return -1;
        }
        count as isize
    }

    /// Reads into `buf`, `len` bytes at most.
    unsafe fn read_into(fd: c_int, buf: *mut u8, len: usize) -> isize {
        let iov = Iovec {
            buf,
            len: len.min(MOST_READ),
        };
        let mut read = 0;
        let error = unsafe { __wasi_fd_read(fd as u32, &iov, 1, &mut read) };
        answer(error, read)
    }

    /// Writes from `buf`, `len` bytes at most.
    unsafe fn write_from(fd: c_int, buf: *const u8, len: usize) -> isize {
        let iov = Iovec {
            buf: buf.cast_mut(),
            len: len.min(MOST_WRITTEN),
        };
        let mut written = 0;
        let error = unsafe { __wasi_fd_write(fd as u32, &iov, 1, &mut written) };
        answer(error, written)
    }

    /// The first of `count` buffers with room, the one a call on all of
    /// them would start with; None when none has.
    unsafe fn first(iov: *const Iovec, count: c_int) -> Result<Option<(*mut u8, usize)>, ()> {
        if count < 0 {
            unsafe { *__errno_location() = EINVAL };
            return Err(());
        }
        let iovs = if count == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(iov, count as usize) }
        };
        Ok(iovs
            .iter()
            .find(|iov| iov.len > 0)
            .map(|iov| (iov.buf, iov.len)))
    }

    /// POSIX's `read`, in place of wasi-libc's.
    ///
    /// # Safety
    ///
    /// As `read`: `buf` holds `count` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn read(fd: c_int, buf: *mut u8, count: usize) -> isize {
        unsafe { read_into(fd, buf, count) }
    }

    /// POSIX's `readv`, in place of wasi-libc's: into the first buffer with
    /// room.
    ///
    /// # Safety
    ///
    /// As `readv`: `iov` holds `count` buffers.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn readv(fd: c_int, iov: *const Iovec, count: c_int) -> isize {
        match unsafe { first(iov, count) } {
            Err(()) => -1,
            Ok(Some((buf, len))) => unsafe { read_into(fd, buf, len) },
            Ok(None) => 0,
        }
    }

    /// POSIX's `write`, in place of wasi-libc's.
    ///
    /// # Safety
    ///
    /// As `write`: `buf` holds `count` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn write(fd: c_int, buf: *const u8, count: usize) -> isize {
        unsafe { write_from(fd, buf, count) }
    }

    /// POSIX's `writev`, in place of wasi-libc's: from the first buffer
    /// with something in it.
    ///
    /// # Safety
    ///
    /// As `writev`: `iov` holds `count` buffers.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn writev(fd: c_int, iov: *const Iovec, count: c_int) -> isize {
        match unsafe { first(iov, count) } {
            Err(()) => -1,
            Ok(Some((buf, len))) => unsafe { write_from(fd, buf, len) },
            Ok(None) => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::{IoSlice, IoSliceMut, Read, Write};

    use super::{MOST_READ, MOST_WRITTEN};

    /// Under rootshell.py, which fails a read too big for rootshell, as
    /// rootshell does, these only pass with this module's reads.
    #[test]
    fn reads_and_writes_a_big_file_whole() {
        let folder = crate::testing::scratch("chunks");
        let path = folder.join("big");
        let bytes: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);

        let mut file = File::open(&path).unwrap();
        let mut buffer = vec![0u8; 1 << 20];
        let first = file.read(&mut buffer).unwrap();
        assert!(first > 0);
        assert_eq!(buffer[..first], bytes[..first]);

        let (mut empty, mut big) = ([0u8; 0], vec![0u8; 1 << 20]);
        let second = file
            .read_vectored(&mut [IoSliceMut::new(&mut empty), IoSliceMut::new(&mut big)])
            .unwrap();
        assert!(second > 0);
        assert_eq!(big[..second], bytes[first..][..second]);
        if cfg!(target_os = "wasi") {
            assert!(first <= MOST_READ && second <= MOST_READ);
        }

        let copy = folder.join("copy");
        let mut file = File::create(&copy).unwrap();
        let written = file.write(&bytes).unwrap();
        assert!(written > 0);
        let more = file
            .write_vectored(&[IoSlice::new(&[]), IoSlice::new(&bytes[written..])])
            .unwrap();
        if cfg!(target_os = "wasi") {
            assert!(written <= MOST_WRITTEN && more > 0 && more <= MOST_WRITTEN);
        }
        file.write_all(&bytes[written + more..]).unwrap();
        drop(file);
        assert_eq!(fs::read(&copy).unwrap(), bytes);
        fs::remove_dir_all(folder).unwrap();
    }
}
