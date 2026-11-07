//! One Syncative process per data dir.
//!
//! Two instances on the same SQLite DB would run overlapping sync passes over the same trees. The first process takes an exclusive `flock` on `<data_dir>/instance.lock` and listens on `<data_dir>/instance.sock`; a later launch (e.g. the app-menu entry while the autostarted copy sits in the tray) fails the lock, asks the running instance to show its window over the socket, and exits. A launch with `--save <files>` (the file manager's context menu) sends `save`, a newline and the paths separated by NUL bytes instead, and the running instance asks where to save them.

use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use iced::{stream, Subscription};

/// Listener handed from `acquire` to the subscription. The subscription builder is a plain `fn`, so the hand-off goes through a static.
static LISTENER: OnceLock<Mutex<Option<UnixListener>>> = OnceLock::new();

/// Held for the whole process lifetime; dropping it would release the lock.
static LOCK_FILE: OnceLock<File> = OnceLock::new();

pub enum Instance {
    /// We own the data dir.
    Primary,
    /// Another process owns it and has been asked to show its window.
    Secondary,
}

/// Take the data dir, or hand over to the process that has it: ask it to show its window, or to save `save` in the cloud.
pub fn acquire(data_dir: &Path, save: &[PathBuf]) -> Instance {
    let sock_path = data_dir.join("instance.sock");
    let lock = match File::options().create(true).truncate(false).write(true).open(data_dir.join("instance.lock")) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("syncative: couldn't open instance lock ({err}); continuing without single-instance guard.");
            return Instance::Primary;
        }
    };
    // SAFETY: plain syscall on a valid, owned fd.
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        let mut request = Vec::new();
        if save.is_empty() {
            request.extend_from_slice(b"show\n");
        } else {
            request.extend_from_slice(b"save\n");
            for path in save {
                request.extend_from_slice(path.as_os_str().as_bytes());
                request.push(0);
            }
        }
        match UnixStream::connect(&sock_path).and_then(|mut s| s.write_all(&request)) {
            Ok(()) => eprintln!("syncative: already running — asked the running instance to show its window."),
            Err(err) => eprintln!("syncative: already running, but couldn't reach it ({err})."),
        }
        return Instance::Secondary;
    }
    let _ = LOCK_FILE.set(lock);
    // A stale socket from a crashed run would make `bind` fail.
    let _ = std::fs::remove_file(&sock_path);
    match UnixListener::bind(&sock_path) {
        Ok(listener) => {
            let _ = LISTENER.set(Mutex::new(Some(listener)));
        }
        Err(err) => eprintln!("syncative: couldn't bind {} ({err}); a second launch won't be able to raise this window.", sock_path.display()),
    }
    Instance::Primary
}

/// Emits `()` whenever another launch asks this instance to show its window, also after it handed over files to save (see [`crate::services::share::receive_files`]).
pub fn show_requests() -> Subscription<()> {
    Subscription::run(|| {
        stream::channel(4, async move |mut output| {
            use iced::futures::SinkExt;
            let listener = LISTENER.get().and_then(|m| m.lock().ok()?.take());
            if let Some(listener) = listener {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(4);
                std::thread::spawn(move || {
                    for mut stream in listener.incoming().flatten() {
                        let mut request = Vec::new();
                        if stream.read_to_end(&mut request).is_err() {
                            continue;
                        }
                        if let Some(paths) = request.strip_prefix(b"save\n") {
                            #[cfg(not(target_os = "android"))]
                            crate::services::share::receive_files(paths.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| PathBuf::from(OsStr::from_bytes(p))).collect());
                            #[cfg(target_os = "android")]
                            let _ = paths;
                        } else if request.trim_ascii() != b"show" {
                            continue;
                        }
                        if tx.blocking_send(()).is_err() {
                            break;
                        }
                    }
                });
                while rx.recv().await.is_some() {
                    let _ = output.send(()).await;
                }
            }
            std::future::pending::<()>().await;
        })
    })
}
