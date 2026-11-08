//! One window: launching Margin again (from a file manager, a terminal) opens the files as tabs in
//! the window that's already there.

use std::io::{Read, Write};
use std::path::PathBuf;

use interprocess::local_socket::traits::{ListenerExt, Stream as _};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream, ToNsName};

fn name() -> String {
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
    format!("margin-{user}.sock")
}

/// Hands `paths` to a running Margin. True if one took them.
pub fn hand_over(paths: &[PathBuf]) -> bool {
    let Ok(n) = name().to_ns_name::<GenericNamespaced>() else { return false };
    let Ok(mut s) = Stream::connect(n) else { return false };
    let list: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let Ok(msg) = serde_json::to_vec(&list) else { return false };
    s.write_all(&msg).is_ok() && s.flush().is_ok()
}

/// Listens for later launches; each sends the files it was given (maybe none: "show yourself").
pub fn listen(on_open: impl Fn(Vec<PathBuf>) + Send + 'static) {
    let Ok(n) = name().to_ns_name::<GenericNamespaced>() else { return };
    let Ok(listener) = ListenerOptions::new().name(n).create_sync() else { return };
    std::thread::Builder::new()
        .name("margin-ipc".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut conn = conn;
                let mut buf = Vec::new();
                if conn.read_to_end(&mut buf).is_ok()
                    && let Ok(list) = serde_json::from_slice::<Vec<String>>(&buf)
                {
                    on_open(list.into_iter().map(PathBuf::from).collect());
                }
            }
        })
        .ok();
}
