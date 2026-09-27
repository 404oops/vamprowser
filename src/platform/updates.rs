//! Keeping extensions current: once a day (and on request) each installed
//! add-on is looked up on addons.mozilla.org, and a newer version replaces
//! it in place, keeping its storage. The compatibility pass runs on every
//! install, so updates keep working in WebKit.

use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

use gpui::Context;

use crate::{
    Browser, BrowserEvent,
    extensions::{Extensions, Prepared},
};

/// How often extensions are checked for updates.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// What a check found: updates downloaded and ready, and what failed.
#[derive(Debug)]
pub(crate) struct UpdateReport {
    /// Unpacked and patched here, off the main thread.
    pub found: Vec<(String, String, Prepared)>,
    pub errors: Vec<String>,
    /// Asked for from the settings page, so it says so even if nothing's new.
    pub manual: bool,
}

fn stamp() -> Option<PathBuf> {
    crate::state::data_path("Extensions/.last-update-check")
}

/// Whether a day has passed since the last check.
pub(crate) fn due() -> bool {
    stamp()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_none_or(|age| age >= EVERY)
}

impl Browser {
    /// Looks every installed extension up on addons.mozilla.org, off the
    /// main thread; the answer comes back as `ExtensionUpdates`.
    pub(crate) fn check_extension_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        let running = self
            .common
            .checking_updates
            .get()
            .is_some_and(|since| since.elapsed() < Duration::from_secs(10 * 60));
        if running {
            if manual {
                self.notice = Some("Already checking for extension updates…".into());
                cx.notify();
            }
            return;
        }
        self.common.checking_updates.set(Some(std::time::Instant::now()));
        let installed: Vec<(String, String, String)> = {
            let guard = self.common.extensions.borrow();
            let Some(extensions) = guard.as_ref() else {
                self.common.checking_updates.set(None);
                return;
            };
            extensions
                .list()
                .into_iter()
                .map(|info| (info.id, info.name, info.version))
                .collect()
        };
        if manual {
            self.notice = Some("Checking for extension updates…".into());
            cx.notify();
        }
        let sender = self.common.anywhere.clone();
        std::thread::spawn(move || {
            let mut report = UpdateReport {
                found: Vec::new(),
                errors: Vec::new(),
                manual,
            };
            for (id, name, version) in installed {
                match Extensions::newer_on_amo(&id, &version) {
                    Ok(Some((latest, path))) => {
                        match Extensions::prepare(&path) {
                            Ok(prepared) => {
                                report.found.push((name, latest, prepared.replacing(version)))
                            }
                            Err(err) => report.errors.push(format!("{name}: {err}")),
                        }
                        let _ = std::fs::remove_file(path);
                    }
                    Ok(None) => {}
                    // Not on addons.mozilla.org (installed from a file):
                    // nothing to update from, and nothing worth saying.
                    Err(err) if err.contains("404") => {}
                    Err(err) => report.errors.push(format!("{name}: {err}")),
                }
            }
            if let Some(path) = stamp() {
                let _ = std::fs::write(path, b"");
            }
            let _ = sender.try_send(BrowserEvent::ExtensionUpdates(report));
        });
    }

    /// Installs what a check found, each over its old version.
    pub(crate) fn install_extension_updates(&mut self, report: UpdateReport, cx: &mut Context<Self>) {
        self.common.checking_updates.set(None);
        let mut updated = Vec::new();
        let mut errors = report.errors;
        {
            let mut guard = self.common.extensions.borrow_mut();
            if let Some(extensions) = guard.as_mut() {
                for (name, version, prepared) in report.found {
                    match extensions.install_prepared(prepared) {
                        Ok(Some(_)) => updated.push(format!("{name} {version}")),
                        // Removed or changed while the update downloaded.
                        Ok(None) => {}
                        Err(err) => errors.push(format!("{name}: {err}")),
                    }
                }
            }
        }
        for error in &errors {
            eprintln!("[extension update] {error}");
        }
        if !updated.is_empty() {
            eprintln!("Updated extensions: {}", updated.join(", "));
            self.notice = Some(format!("Updated {}.", updated.join(", ")));
        } else if report.manual {
            self.notice = Some(if errors.is_empty() {
                "Your extensions are up to date.".into()
            } else {
                format!("Couldn't check every extension: {}", errors.join("; "))
            });
        }
        self.refresh_other_windows(cx);
        cx.notify();
    }
}
