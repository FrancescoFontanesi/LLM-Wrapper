//! Files going in and out of the page: downloads and the upload chooser.
//!
//! Nothing moves without the user choosing the file in a dialog (the desktop's own, through the
//! file-chooser portal when there is one): a page cannot write anywhere by itself, nor read a file
//! it was not handed. One dialog at a time — a page that asks for a hundred downloads gets one
//! dialog and ninety-nine refusals, not a hundred windows.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk4::gio::{self, prelude::*};
use gtk4::glib;
use webkit6::{Download, FileChooserRequest, NetworkSession};

use crate::{log, notify};

/// Longest file name offered in the "save" dialog.
const MAX_NAME_BYTES: usize = 200;

/// The name to offer for a download the server calls `suggested`: its last path component, without
/// control characters, never empty, and never `.` or `..`.
pub fn dialog_file_name(suggested: &str) -> String {
    let last = suggested.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last.chars().filter(|c| !c.is_control()).collect();
    let mut name = cleaned.trim().to_owned();
    while name.len() > MAX_NAME_BYTES {
        name.pop();
    }
    if name.is_empty() || name == "." || name == ".." {
        "download".to_owned()
    } else {
        name
    }
}

/// Marks a dialog as open for as long as it lives.
struct Busy(Rc<Cell<bool>>);

impl Busy {
    fn take(flag: &Rc<Cell<bool>>) -> Option<Self> {
        if flag.replace(true) {
            None
        } else {
            Some(Self(flag.clone()))
        }
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// Downloads and uploads for one window.
pub struct Transfers {
    window: gtk4::Window,
    dialog_open: Rc<Cell<bool>>,
    /// Downloads started and not finished.
    downloads: Rc<Cell<usize>>,
}

impl Transfers {
    pub fn new(window: gtk4::Window) -> Rc<Self> {
        Rc::new(Self {
            window,
            dialog_open: Rc::default(),
            downloads: Rc::default(),
        })
    }

    /// Is a file dialog open, or a download running? Then the page is in use.
    pub fn engaged(&self) -> Option<crate::reap::Busy> {
        if self.dialog_open.get() {
            Some(crate::reap::Busy::Dialog)
        } else if self.downloads.get() > 0 {
            Some(crate::reap::Busy::Download)
        } else {
            None
        }
    }

    /// Handle every download the session starts.
    pub fn attach_downloads(self: &Rc<Self>, session: &NetworkSession) {
        let me = self.clone();
        session.connect_download_started(move |_, download| me.on_download(download));
    }

    fn on_download(self: &Rc<Self>, download: &Download) {
        let me = self.clone();
        download.connect_decide_destination(move |download, suggested| {
            me.choose_destination(download, suggested);
            true
        });
        self.downloads.set(self.downloads.get() + 1);
        // `failed` comes first and `finished` always follows, success or not
        let failed = Rc::new(Cell::new(false));
        let flag = failed.clone();
        let count = self.downloads.clone();
        download.connect_finished(move |download| {
            count.set(count.get().saturating_sub(1));
            if flag.get() {
                return;
            }
            let name = download
                .destination()
                .and_then(|d| {
                    Path::new(d.as_str())
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "File".to_owned());
            notify::send("Download complete", &format!("Saved {name}"));
        });
        download.connect_failed(move |_, error| {
            failed.set(true);
            // the user closing the dialog is not news
            if !error.matches(webkit6::DownloadError::CancelledByUser) {
                notify::send("Download failed", error.message());
            }
        });
    }

    /// Ask where to save, then tell the download. Answered later, so the handler claims the signal.
    fn choose_destination(self: &Rc<Self>, download: &Download, suggested: &str) {
        let Some(busy) = Busy::take(&self.dialog_open) else {
            log::warn("a download was refused: another file dialog is already open");
            download.cancel();
            return;
        };
        let (window, download) = (self.window.clone(), download.clone());
        let name = dialog_file_name(suggested);
        glib::spawn_future_local(async move {
            let _busy = busy;
            let dialog = gtk4::FileDialog::builder()
                .title("Save File")
                .accept_label("Save")
                .modal(true)
                .initial_name(name)
                .build();
            if let Some(dir) = glib::user_special_dir(glib::UserDirectory::Downloads) {
                if dir.is_dir() {
                    dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
                }
            }
            let Ok(file) = dialog.save_future(Some(&window)).await else {
                download.cancel(); // dismissed
                return;
            };
            let path = file.path();
            let Some(path) = path.as_deref().and_then(Path::to_str) else {
                log::warn("a download was refused: the chosen place is not a local file");
                download.cancel();
                return;
            };
            // the dialog has already asked about overwriting
            download.set_allow_overwrite(true);
            download.set_destination(path);
        });
    }

    /// The page asked for files: let the user pick them.
    pub fn choose_upload(self: &Rc<Self>, request: &FileChooserRequest) {
        let Some(busy) = Busy::take(&self.dialog_open) else {
            log::warn("an upload dialog was refused: another file dialog is already open");
            request.cancel();
            return;
        };
        let (window, request) = (self.window.clone(), request.clone());
        glib::spawn_future_local(async move {
            let _busy = busy;
            let dialog = gtk4::FileDialog::builder()
                .title("Select File to Upload")
                .accept_label("Open")
                .modal(true)
                .build();
            if let Some(filter) = request.mime_types_filter() {
                let filters = gio::ListStore::new::<gtk4::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_default_filter(Some(&filter));
            }
            let chosen: Result<Vec<gio::File>, glib::Error> = if request.selects_multiple() {
                dialog
                    .open_multiple_future(Some(&window))
                    .await
                    .map(|list| {
                        (0..list.n_items())
                            .filter_map(|i| list.item(i).and_downcast::<gio::File>())
                            .collect()
                    })
            } else {
                dialog
                    .open_future(Some(&window))
                    .await
                    .map(|file| vec![file])
            };
            let paths: Vec<PathBuf> = chosen
                .unwrap_or_default()
                .iter()
                .filter_map(gio::File::path)
                .collect();
            let paths: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
            if paths.is_empty() {
                request.cancel();
            } else {
                request.select_files(&paths);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offered_name_is_a_plain_file_name() {
        assert_eq!(dialog_file_name("report.pdf"), "report.pdf");
        assert_eq!(dialog_file_name("../../etc/passwd"), "passwd");
        assert_eq!(dialog_file_name("C:\\Users\\x\\evil.exe"), "evil.exe");
        assert_eq!(dialog_file_name("a/b/"), "download");
        assert_eq!(dialog_file_name(".."), "download");
        assert_eq!(dialog_file_name("."), "download");
        assert_eq!(dialog_file_name(""), "download");
        assert_eq!(dialog_file_name("na\u{0}me\u{1b}[0m.txt"), "name[0m.txt");
        assert_eq!(dialog_file_name("  spaced name.txt  "), "spaced name.txt");
        assert!(dialog_file_name(&"n".repeat(1000)).len() <= MAX_NAME_BYTES);
    }

    #[test]
    fn only_one_dialog_at_a_time() {
        let flag = Rc::new(Cell::new(false));
        let first = Busy::take(&flag).expect("free");
        assert!(
            Busy::take(&flag).is_none(),
            "a second dialog while one is open"
        );
        drop(first);
        assert!(Busy::take(&flag).is_some(), "free again once closed");
    }
}
