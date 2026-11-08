//! Files the engine keeps (settings.json, accounts.json), replaced whole so readers in other
//! processes never see half a file.
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Writes `data` to a temporary file next to `path`, then renames it over `path`, creating
/// `directory` first.
pub fn replace(directory: &Path, path: &Path, data: &[u8]) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(directory)?;
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("file");
    let temporary =
        directory.join(format!(".{name}.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    if let Err(error) = std::fs::write(&temporary, data).and_then(|()| std::fs::rename(&temporary, path)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

/// An absolute `path` as the path of a `file:` URL, before percent-encoding: itself on Unix,
/// `/C:/Users/…` on Windows.
pub fn url_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) { format!("/{}", text.replace('\\', "/")) } else { text.into_owned() }
}
