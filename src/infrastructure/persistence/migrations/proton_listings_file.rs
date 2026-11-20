//! One-shot startup migration: removes the Proton listing cache files `<data>/cache/proton-listings-<remote id>.json` that versions up to 0.26.4 kept. The database took their place, and they hold decrypted file and folder names. Nothing is carried over: the first pass after the update lists each Proton folder once.

use std::path::Path;

pub fn run(data_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(data_dir.join("cache")) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("proton-listings-") && name.ends_with(".json")) {
            continue;
        }
        if let Err(err) = std::fs::remove_file(entry.path()) {
            eprintln!(
                "syncative: could not remove the old Proton listing cache {}: {err}",
                entry.path().display()
            );
        }
    }
}
