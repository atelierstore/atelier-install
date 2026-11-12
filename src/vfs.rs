//! The file access isideload goes through (the isideload-vfs crate), as the
//! system's: the crate's own only builds off wasm32, which leaves a WASI
//! program with none. Its temporary folder, where isideload unpacks the IPA
//! and signs it, is the program's work folder: WASI has no temporary
//! folder, and std's `temp_dir` panics there.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use isideload_vfs::{OpenOptionsConfig, Vfs, VfsFile, VfsMetadata, VfsPermissions};

pub struct StdVfs {
    temp: PathBuf,
}

impl StdVfs {
    pub fn new(temp: PathBuf) -> Self {
        StdVfs { temp }
    }
}

#[derive(Debug)]
struct StdFile(fs::File);

impl Read for StdFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for StdFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Seek for StdFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }
}

impl VfsFile for StdFile {
    fn set_len(&self, size: u64) -> io::Result<()> {
        self.0.set_len(size)
    }

    fn metadata(&self) -> io::Result<Box<dyn VfsMetadata>> {
        Ok(Box::new(StdMetadata(self.0.metadata()?)))
    }

    fn set_permissions(&mut self, permissions: Box<dyn VfsPermissions>) -> io::Result<()> {
        let mut own = self.0.metadata()?.permissions();
        own.set_readonly(permissions.readonly());
        self.0.set_permissions(own)
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.0.sync_all()
    }
}

struct StdMetadata(fs::Metadata);

impl VfsMetadata for StdMetadata {
    fn is_dir(&self) -> bool {
        self.0.is_dir()
    }

    fn is_file(&self) -> bool {
        self.0.is_file()
    }

    fn is_symlink(&self) -> bool {
        self.0.file_type().is_symlink()
    }

    fn len(&self) -> u64 {
        self.0.len()
    }

    fn permissions(&self) -> Box<dyn VfsPermissions> {
        Box::new(StdPermissions(self.0.permissions()))
    }
}

/// Only whether a file is read-only: WASI has no modes.
struct StdPermissions(fs::Permissions);

impl VfsPermissions for StdPermissions {
    fn readonly(&self) -> bool {
        self.0.readonly()
    }

    fn set_readonly(&mut self, readonly: bool) {
        self.0.set_readonly(readonly);
    }

    fn mode(&self) -> u32 {
        if self.0.readonly() { 0o444 } else { 0o644 }
    }

    fn set_mode(&mut self, mode: u32) {
        self.0.set_readonly(mode & 0o222 == 0);
    }
}

impl Vfs for StdVfs {
    fn open_file(&self, path: &Path, options: &OpenOptionsConfig) -> io::Result<Box<dyn VfsFile>> {
        let file = fs::OpenOptions::new()
            .read(options.read)
            .write(options.write)
            .create(options.create)
            .create_new(options.create_new)
            .append(options.append)
            .truncate(options.truncate)
            .open(path)?;
        Ok(Box::new(StdFile(file)))
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        fs::write(path, contents)
    }

    fn copy(&self, from: &Path, to: &Path) -> io::Result<u64> {
        fs::copy(from, to)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        fs::create_dir(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        fs::read_link(path)
    }

    fn set_permissions(&self, path: &Path, permissions: Box<dyn VfsPermissions>) -> io::Result<()> {
        let mut own = fs::metadata(path)?.permissions();
        own.set_readonly(permissions.readonly());
        fs::set_permissions(path, own)
    }

    fn metadata(&self, path: &Path) -> io::Result<Box<dyn VfsMetadata>> {
        Ok(Box::new(StdMetadata(fs::metadata(path)?)))
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Box<dyn VfsMetadata>> {
        Ok(Box::new(StdMetadata(fs::symlink_metadata(path)?)))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }

    fn temp_dir(&self) -> PathBuf {
        self.temp.clone()
    }

    /// rootshell's folder holds no links.
    fn symlink(&self, _target: &Path, _link: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "rootshell's folder holds no symbolic links",
        ))
    }
}
