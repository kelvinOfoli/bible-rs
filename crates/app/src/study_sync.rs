use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use bible_core::study::StudyStore;
use bible_core::study::sync::{LibraryFolder, SyncRequest, SyncResponse};
use eframe::egui;

enum Completed {
    Folder(Result<Option<LibraryFolder>, String>),
    Exchange(Result<Option<SyncResponse>, String>),
}

pub struct StudySync {
    tx: Sender<Completed>,
    rx: Receiver<Completed>,
    working: bool,
    last: Option<Instant>,
    retry: bool,
}

impl Default for StudySync {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, working: false, last: None, retry: false }
    }
}

impl StudySync {
    pub fn choose(&mut self, create: bool, ctx: &egui::Context) -> Result<()> {
        if self.working {
            bail!("A library operation is still running. Try again when it finishes");
        }
        self.working = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<Option<LibraryFolder>> {
                let title = if create {
                    "Choose an empty folder for your Study Library"
                } else {
                    "Open an existing Study Library"
                };
                let Some(path) = rfd::FileDialog::new().set_title(title).pick_folder() else {
                    return Ok(None);
                };
                Ok(Some(coordinate_folder(&path, || {
                    if create { LibraryFolder::plan_create(&path) } else { LibraryFolder::open(&path) }
                })?))
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Completed::Folder(result));
            ctx.request_repaint();
        });
        Ok(())
    }

    pub fn disconnect(&mut self, study: &mut StudyStore) -> Result<()> {
        if self.working {
            bail!("A library operation is still running. Try again when it finishes");
        }
        study.disconnect_study_library()?;
        study.set_setting(
            "study-library-status",
            "Disconnected. Study data remains on this device and in the shared folder.",
        )
    }

    pub fn retry(&mut self) {
        self.retry = true;
    }

    pub fn tick(&mut self, study: &mut StudyStore, ctx: &egui::Context) -> Result<()> {
        while let Ok(done) = self.rx.try_recv() {
            self.working = false;
            let result = match done {
                Completed::Folder(Ok(None)) | Completed::Exchange(Ok(None)) => continue,
                Completed::Folder(Ok(Some(folder))) => match study.prepare_library_connection(folder) {
                    Ok(request) => {
                        self.exchange(request, true, ctx);
                        continue;
                    }
                    Err(e) => Err(format!("{e:#}")),
                },
                Completed::Folder(Err(e)) => Err(e),
                Completed::Exchange(Ok(Some(response))) => {
                    study.finish_library_sync(response).map_err(|e| format!("{e:#}"))
                }
                Completed::Exchange(Err(e)) => Err(e),
            };
            let message = match result {
                Ok(()) => {
                    "Saved on this device. Shared folder checked; cloud delivery may still be pending.".into()
                }
                Err(e) => {
                    format!("Needs attention: {e}. Local edits are kept. Retry or reconnect the folder.")
                }
            };
            study.set_setting("study-library-status", &message)?;
        }
        let connected = study.study_library()?.is_some();
        if connected {
            ctx.request_repaint_after(Duration::from_secs(5));
        }
        if self.working
            || !connected
            || (!self.retry && self.last.is_some_and(|last| last.elapsed() < Duration::from_secs(5)))
        {
            return Ok(());
        }
        self.retry = false;
        self.last = Some(Instant::now());
        let Some(request) = study.prepare_library_sync()? else { return Ok(()) };
        self.exchange(request, false, ctx);
        Ok(())
    }

    fn exchange(&mut self, request: SyncRequest, confirm: bool, ctx: &egui::Context) {
        self.working = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let path = request.folder().path().to_path_buf();
            let result = (|| -> Result<Option<SyncResponse>> {
                if confirm {
                    let preview = coordinate_folder(&path, || request.preview())?;
                    let message = format!("{}\n\n{}\n\nMerge both libraries? A local recovery backup is saved. Conflicting versions are retained.",path.display(),preview.summary());
                    if rfd::MessageDialog::new().set_title("Review Study Library merge").set_description(&message)
                        .set_buttons(rfd::MessageButtons::OkCancel).show() != rfd::MessageDialogResult::Ok { return Ok(None); }
                }
                coordinate_folder(&path, || request.exchange()).map(Some)
            })().map_err(|e| format!("{e:#}"));
            let _ = tx.send(Completed::Exchange(result));
            ctx.request_repaint();
        });
    }
}

#[cfg(not(target_os = "macos"))]
fn coordinate_folder<T>(_path: &std::path::Path, action: impl FnOnce() -> Result<T>) -> Result<T> {
    action()
}

#[cfg(target_os = "macos")]
fn coordinate_folder<T>(path: &std::path::Path, action: impl FnOnce() -> Result<T>) -> Result<T> {
    use objc2_foundation::{NSFileCoordinator, NSFileCoordinatorWritingOptions, NSString, NSURL};
    use std::cell::RefCell;
    let path = path.to_str().ok_or_else(|| anyhow::anyhow!("The folder path is not valid Unicode"))?;
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), true);
    let action = RefCell::new(Some(action));
    let result = RefCell::new(None);
    let accessor = block2::RcBlock::new(|coordinated: std::ptr::NonNull<NSURL>| {
        // SAFETY: Foundation supplies a live NSURL for the duration of this accessor.
        let coordinated = unsafe { coordinated.as_ref() };
        if coordinated.path().as_deref() != url.path().as_deref() {
            *result.borrow_mut() = Some(Err(anyhow::anyhow!(
                "The selected folder moved while waiting for access. Reopen its new location"
            )));
            return;
        }
        if let Some(action) = action.borrow_mut().take() {
            *result.borrow_mut() = Some(action());
        }
    });
    let mut error = None;
    let coordinator = NSFileCoordinator::new();
    coordinator.coordinateWritingItemAtURL_options_error_byAccessor(
        &url,
        NSFileCoordinatorWritingOptions::ForMerging,
        Some(&mut error),
        &accessor,
    );
    drop(accessor);
    if let Some(error) = error {
        bail!("Could not access the Study Library: {error}");
    }
    result
        .into_inner()
        .ok_or_else(|| anyhow::anyhow!("The file provider did not grant access to the Study Library"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_folder_coordination_creates_and_exchanges_a_library() {
        let root = std::env::temp_dir().join(format!(
            "bible-coordination-{}-{}",
            std::process::id(),
            bible_core::study::now()
        ));
        let path = root.join("shared");
        std::fs::create_dir_all(&path).unwrap();
        let folder = coordinate_folder(&path, || LibraryFolder::create(&path)).unwrap();
        let mut study = StudyStore::open(&root.join("study.sqlite")).unwrap();
        study.connect_study_library(folder).unwrap();
        let request = study.prepare_library_sync().unwrap().unwrap();
        let response = coordinate_folder(&path, || request.exchange()).unwrap();
        study.finish_library_sync(response).unwrap();
        drop(study);
        std::fs::remove_dir_all(root).unwrap();
    }
}
