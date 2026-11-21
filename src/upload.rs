//! The signed app's upload to the iPhone's PublicStaging, as it goes, kept
//! across tries. The connection breaks when rootshell has been off screen
//! a while (an iPhone, 12 and 20 Nov 2026): the install then tries again
//! by itself, once rootshell is back, and the upload goes on from where it
//! broke. The files the iPhone has whole stay as they are, and the one it
//! was writing goes on from the last piece the iPhone said it wrote, cut
//! back to that, as it may have written another and not said so before the
//! connection went.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A file of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Its path in the app, with `/`.
    pub path: String,
    /// Where it is here.
    pub local: PathBuf,
    pub len: u64,
}

/// The app's folders, each before what's in it, and its files, by their
/// paths in the app, in the order they go to the iPhone: by name.
pub fn list(app: &Path) -> io::Result<(Vec<String>, Vec<File>)> {
    let (mut folders, mut files) = (Vec::new(), Vec::new());
    walk(app, "", &mut folders, &mut files)?;
    Ok((folders, files))
}

fn walk(
    folder: &Path,
    inside: &str,
    folders: &mut Vec<String>,
    files: &mut Vec<File>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(folder)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort();
    for local in entries {
        let name = local
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = if inside.is_empty() {
            name
        } else {
            format!("{inside}/{name}")
        };
        let meta = fs::metadata(&local)?;
        if meta.is_dir() {
            folders.push(path.clone());
            walk(&local, &path, folders, files)?;
        } else {
            files.push(File {
                path,
                local,
                len: meta.len(),
            });
        }
    }
    Ok(())
}

/// The app's upload, and how far it got.
#[derive(Debug, Default)]
pub struct Upload {
    /// The app's folders and files, from the first try on.
    listed: Option<(Vec<String>, Vec<File>)>,
    /// The files the iPhone has whole, by their paths in the app.
    whole: BTreeSet<String>,
    /// The file going, and how many of its bytes the iPhone said it wrote.
    going: Option<(String, u64)>,
}

impl Upload {
    /// The app's folders and files, listed at the first try.
    pub fn list(&mut self, app: &Path) -> io::Result<(Vec<String>, Vec<File>)> {
        if self.listed.is_none() {
            self.listed = Some(list(app)?);
        }
        Ok(self.listed.clone().unwrap_or_default())
    }

    /// Whether the iPhone has `file` whole.
    pub fn has(&self, file: &File) -> bool {
        self.whole.contains(&file.path)
    }

    /// Whether `file` was going when the connection broke: its size on the
    /// iPhone tells where it can go on from.
    pub fn broke_in(&self, file: &File) -> bool {
        self.going
            .as_ref()
            .is_some_and(|(path, _)| *path == file.path)
    }

    /// Where `file` starts: where it broke, when it was going then and the
    /// iPhone has at least that much of it (`on_iphone`), or else at its
    /// start.
    pub fn start(&self, file: &File, on_iphone: Option<u64>) -> u64 {
        match &self.going {
            Some((path, wrote))
                if *path == file.path
                    && *wrote <= file.len
                    && on_iphone.is_some_and(|size| size >= *wrote) =>
            {
                *wrote
            }
            _ => 0,
        }
    }

    /// `file` goes, from `from`.
    pub fn begin(&mut self, file: &File, from: u64) {
        self.going = Some((file.path.clone(), from));
    }

    /// How many bytes of the file going the iPhone has.
    pub fn going_from(&self) -> u64 {
        self.going.as_ref().map_or(0, |(_, wrote)| *wrote)
    }

    /// The iPhone wrote `len` more bytes of the file going.
    pub fn wrote(&mut self, len: u64) {
        if let Some((_, wrote)) = &mut self.going {
            *wrote += len;
        }
    }

    /// The iPhone has the file going whole.
    pub fn finished(&mut self) {
        if let Some((path, _)) = self.going.take() {
            self.whole.insert(path);
        }
    }

    /// The install has the upload: the iPhone may move it from
    /// PublicStaging, so a next try sends it all again.
    pub fn handed_over(&mut self) {
        self.whole.clear();
        self.going = None;
    }

    /// Where the bar is, out of 100: the upload takes it to 70, and the
    /// install from there.
    pub fn percent(&self) -> u64 {
        let files = self.listed.as_ref().map_or(&[][..], |(_, files)| files);
        let total = files.iter().map(|file| file.len).sum::<u64>();
        let sent = files
            .iter()
            .map(|file| match &self.going {
                _ if self.has(file) => file.len,
                Some((path, wrote)) if *path == file.path => *wrote,
                _ => 0,
            })
            .sum::<u64>();
        sent * 70 / total.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, len: u64) -> File {
        File {
            path: path.into(),
            local: PathBuf::from(path),
            len,
        }
    }

    #[test]
    fn lists_each_folder_before_whats_in_it_by_name() {
        let app = crate::testing::scratch("upload-list").join("Atelier.app");
        fs::create_dir_all(app.join("Frameworks/Kit.framework")).unwrap();
        fs::write(app.join("Info.plist"), b"plist").unwrap();
        fs::write(app.join("Atelier"), b"binary!").unwrap();
        fs::write(app.join("Frameworks/Kit.framework/Kit"), b"kit").unwrap();
        let (folders, files) = list(&app).unwrap();
        fs::remove_dir_all(&app).unwrap();
        assert_eq!(folders, ["Frameworks", "Frameworks/Kit.framework"]);
        let listed: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.len)).collect();
        assert_eq!(
            listed,
            [
                ("Atelier", 7),
                ("Frameworks/Kit.framework/Kit", 3),
                ("Info.plist", 5)
            ]
        );
    }

    /// An upload of `files`, as the first try lists them.
    fn upload(files: &[File]) -> Upload {
        Upload {
            listed: Some((Vec::new(), files.to_vec())),
            ..Upload::default()
        }
    }

    #[test]
    fn goes_on_from_the_last_piece_the_iphone_said_it_wrote() {
        let files = [
            file("Info.plist", 100),
            file("Atelier", 1500),
            file("x", 400),
        ];
        let mut upload = upload(&files);
        assert_eq!(upload.percent(), 0);
        upload.begin(&files[0], 0);
        upload.wrote(100);
        upload.finished();
        upload.begin(&files[1], 0);
        upload.wrote(500);
        upload.wrote(500);
        // The connection broke there.
        assert!(upload.has(&files[0]) && !upload.has(&files[1]));
        assert!(upload.broke_in(&files[1]) && !upload.broke_in(&files[2]));
        assert_eq!(upload.percent(), 1100 * 70 / 2000);
        // The iPhone wrote a third piece, without saying so: it's cut off.
        assert_eq!(upload.start(&files[1], Some(1500)), 1000);
        assert_eq!(upload.start(&files[1], Some(1000)), 1000);
        // Less than it said, or none: from the start.
        assert_eq!(upload.start(&files[1], Some(999)), 0);
        assert_eq!(upload.start(&files[1], None), 0);
        // Another file starts at its start.
        assert_eq!(upload.start(&files[2], Some(400)), 0);
    }

    #[test]
    fn sends_it_all_again_once_the_install_had_it() {
        let files = [file("Atelier", 10)];
        let mut upload = upload(&files);
        upload.begin(&files[0], 0);
        upload.wrote(10);
        upload.finished();
        assert_eq!(upload.percent(), 70);
        upload.handed_over();
        assert_eq!(upload.percent(), 0);
        assert!(!upload.has(&files[0]));
    }
}
