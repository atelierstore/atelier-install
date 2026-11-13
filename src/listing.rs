//! Folder listings that go to the end.
//!
//! rootshell's `fd_readdir` writes a folder's entries from the first, whole
//! ones only, whatever cookie it's given, and stops at the first that
//! doesn't fit (its Resources/Wasm/wasm-worker.js). wasi-libc's `readdir`
//! takes a buffer that isn't full to be the listing's end, so a folder
//! whose entries take more than its 4 KB buffer was listed in part: 96 of
//! 300 files under harness/rootshell.py. The signing hashes the files it
//! lists, and the install sends them, so an app with a large folder would
//! have gone to the iPhone with files missing.
//!
//! So the program's calls to `__wasi_fd_readdir`, wasi-libc's and std's
//! through it, come here instead (build.rs links them so, with `--wrap`).
//! This reads the folder's whole listing from rootshell, then answers as
//! WASI says `fd_readdir` does: the entries after the cookie's, as many as
//! fit, the last one cut short when the buffer's full, which is how
//! wasi-libc knows to ask for more.

/// An entry's header: `d_next`, `d_ino`, `d_namlen` and `d_type`, padded.
const HEADER: usize = 24;

/// Room enough for the entry after a listing's last: when a read leaves
/// this much of the buffer, nothing was left out. A name has 255 UTF-16
/// units at most on Apple's file systems, so 765 bytes of UTF-8.
const ROOM: usize = HEADER + 1024;

/// The whole entries in `listing`, in order.
fn entries(listing: &[u8]) -> Vec<&[u8]> {
    let mut entries = Vec::new();
    let mut at = 0;
    while let Some(header) = listing.get(at..at + HEADER) {
        let name = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
        let Some(entry) = listing.get(at..at + HEADER + name) else {
            break;
        };
        entries.push(entry);
        at += entry.len();
    }
    entries
}

fn next_cookie(entry: &[u8]) -> u64 {
    u64::from_le_bytes(entry[..8].try_into().unwrap())
}

/// Writes into `out` what `fd_readdir` answers for `cookie`, from a
/// folder's whole `listing`, and returns how much of `out` it wrote. The
/// cookie is the `d_next` of the last entry read, 0 for the start; were
/// that entry gone, the cookie counts entries, as rootshell's do.
fn answer(listing: &[u8], cookie: u64, out: &mut [u8]) -> usize {
    let entries = entries(listing);
    let start = match cookie {
        0 => 0,
        cookie => entries
            .iter()
            .position(|entry| next_cookie(entry) == cookie)
            .map_or(usize::try_from(cookie).unwrap_or(usize::MAX), |at| at + 1),
    };
    let mut at = 0;
    for entry in entries.iter().skip(start) {
        let taken = entry.len().min(out.len() - at);
        out[at..at + taken].copy_from_slice(&entry[..taken]);
        at += taken;
        if at == out.len() {
            break;
        }
    }
    at
}

/// Whether a read that wrote `used` bytes of `size` holds the whole
/// listing.
fn whole(used: usize, size: usize) -> bool {
    size - used.min(size) >= ROOM
}

#[cfg(target_os = "wasi")]
mod wasi {
    unsafe extern "C" {
        /// wasi-libc's own, which calls rootshell's.
        fn __real___wasi_fd_readdir(
            fd: u32,
            buf: *mut u8,
            len: usize,
            cookie: u64,
            used: *mut usize,
        ) -> u16;
    }

    /// `fd_readdir`, as WASI has it, for wasi-libc's calls.
    ///
    /// # Safety
    ///
    /// `buf` holds `len` bytes, and `used` is a `usize`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wrap___wasi_fd_readdir(
        fd: u32,
        buf: *mut u8,
        len: usize,
        cookie: u64,
        used: *mut usize,
    ) -> u16 {
        let mut size = 64 * 1024;
        let listing = loop {
            let mut listing = vec![0u8; size];
            let mut read = 0;
            let error =
                unsafe { __real___wasi_fd_readdir(fd, listing.as_mut_ptr(), size, 0, &mut read) };
            if error != 0 {
                return error;
            }
            if super::whole(read, size) {
                listing.truncate(read);
                break listing;
            }
            size *= 2;
        };
        let out = unsafe { std::slice::from_raw_parts_mut(buf, len) };
        unsafe { *used = super::answer(&listing, cookie, out) };
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An entry as rootshell writes one: `d_next` and `d_ino` its place
    /// from 1, and a file's type.
    fn entry(place: u64, name: &str) -> Vec<u8> {
        let mut entry = Vec::new();
        entry.extend(place.to_le_bytes());
        entry.extend(place.to_le_bytes());
        entry.extend((name.len() as u32).to_le_bytes());
        entry.extend([4, 0, 0, 0]);
        entry.extend(name.as_bytes());
        entry
    }

    fn listing(names: &[&str]) -> Vec<u8> {
        names
            .iter()
            .zip(1..)
            .flat_map(|(name, place)| entry(place, name))
            .collect()
    }

    /// The names a reader gets with a buffer of `size`, reading on from
    /// each last whole entry while the buffer comes back full, as
    /// wasi-libc's `readdir` does.
    fn read_all(listing: &[u8], size: usize) -> Vec<String> {
        let (mut names, mut cookie) = (Vec::new(), 0);
        loop {
            let mut out = vec![0; size];
            let used = answer(listing, cookie, &mut out);
            for entry in entries(&out[..used]) {
                names.push(String::from_utf8(entry[HEADER..].to_vec()).unwrap());
                cookie = next_cookie(entry);
            }
            if used < size {
                return names;
            }
        }
    }

    #[test]
    fn a_small_buffer_reads_the_whole_folder() {
        let names: Vec<String> = (0..300).map(|n| format!("file_number_{n}.png")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let listing = listing(&names);
        assert!(listing.len() > 4096);
        assert_eq!(read_all(&listing, 4096), names);
        // A buffer that ends where an entry does, and one barely bigger
        // than the longest entry.
        let first = entry(1, names[0]).len();
        assert_eq!(read_all(&listing, first * 3), names);
        assert_eq!(read_all(&listing, HEADER + 20), names);
    }

    #[test]
    fn the_last_entry_that_doesnt_fit_is_cut_short() {
        let listing = listing(&["a", "bb", "ccc"]);
        let mut out = vec![0; HEADER + 1 + HEADER + 1];
        assert_eq!(answer(&listing, 0, &mut out), out.len());
        assert_eq!(entries(&out).len(), 1);
        // Reading on from the first.
        let mut out = vec![0; 4096];
        let used = answer(&listing, 1, &mut out);
        assert_eq!(used, 2 * HEADER + 5);
        assert_eq!(answer(&listing, 3, &mut out), 0);
    }

    #[test]
    fn a_cookie_past_the_end_reads_nothing() {
        let listing = listing(&["a"]);
        assert_eq!(answer(&listing, 7, &mut [0; 64]), 0);
        assert_eq!(answer(&listing, u64::MAX, &mut [0; 64]), 0);
    }

    #[test]
    fn a_read_with_room_left_holds_the_whole_listing() {
        assert!(whole(0, 64 * 1024));
        assert!(whole(64 * 1024 - ROOM, 64 * 1024));
        assert!(!whole(64 * 1024 - ROOM + 1, 64 * 1024));
        assert!(!whole(64 * 1024, 64 * 1024));
    }

    /// Under rootshell.py, which lists folders as rootshell does, this
    /// only passes with this module's `fd_readdir`.
    #[test]
    fn a_large_folder_lists_and_deletes_whole() {
        let folder = crate::testing::scratch("listing");
        let inner = folder.join("inner");
        std::fs::create_dir(&inner).unwrap();
        for n in 0..300 {
            std::fs::write(folder.join(format!("file_number_{n}.png")), "").unwrap();
            std::fs::write(inner.join(format!("file_number_{n}.png")), "").unwrap();
        }
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 301);
        assert_eq!(std::fs::read_dir(&inner).unwrap().count(), 300);
        // harness/rootshell.py deletes a folder only when it's empty, as
        // WASI says, where rootshell deletes what's left in it too.
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(!folder.exists());
    }
}
