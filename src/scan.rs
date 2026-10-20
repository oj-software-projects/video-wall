use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v", "mpg", "mpeg", "3gp", "ogv",
];

pub const MIN_SIZE: u64 = 1_048_576;

pub fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn is_playable(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if name.is_empty() || name.starts_with("._") {
        return false;
    }
    if !VIDEO_EXTS.contains(&extension(path).as_str()) {
        return false;
    }
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() >= MIN_SIZE)
        .unwrap_or(false)
}

/// Recursive scan of every folder. Unreadable folders are skipped with a
/// warning; results are deduplicated by resolved path, preserving first hit.
pub fn scan(dirs: &[String]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();

    for dir in dirs {
        let root = PathBuf::from(dir);
        let meta = match fs::metadata(&root) {
            Ok(meta) => meta,
            Err(err) => {
                crate::log::warn("scanner", &format!("skipping {dir}: {err}"));
                continue;
            }
        };
        if !meta.is_dir() {
            crate::log::warn("scanner", &format!("skipping {dir}: not a directory"));
            continue;
        }

        let mut stack = vec![root];
        while let Some(current) = stack.pop() {
            let canonical_dir = fs::canonicalize(&current).unwrap_or_else(|_| current.clone());
            if !seen_dirs.insert(canonical_dir) {
                continue;
            }
            let entries = match fs::read_dir(&current) {
                Ok(entries) => entries,
                Err(err) => {
                    crate::log::warn(
                        "scanner",
                        &format!("cannot read {}: {err}", current.display()),
                    );
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = fs::metadata(&path) else {
                    continue;
                };
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.is_file() && is_playable(&path) {
                    let resolved = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                    if seen_files.insert(resolved) {
                        files.push(path);
                    }
                }
            }
        }
    }

    files
}

pub fn count(dir: &str) -> Result<usize, String> {
    let root = Path::new(dir);
    let meta = fs::metadata(root).map_err(|_| "not found".to_string())?;
    if !meta.is_dir() {
        return Err("not found".to_string());
    }
    if fs::read_dir(root).is_err() {
        return Err("not found".to_string());
    }
    Ok(scan(&[dir.to_string()]).len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tmv-scan-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &Path, len: u64) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = fs::File::create(path).unwrap();
        file.write_all(&vec![0u8; len as usize]).unwrap();
    }

    #[test]
    fn filters_extensions_prefix_and_size() {
        let dir = temp_dir("filter");
        write_file(&dir.join("ok.mp4"), MIN_SIZE);
        write_file(&dir.join("exact.MKV"), MIN_SIZE);
        write_file(&dir.join("small.mp4"), MIN_SIZE - 1);
        write_file(&dir.join("._fork.mp4"), MIN_SIZE);
        write_file(&dir.join("notes.txt"), MIN_SIZE);
        write_file(&dir.join("nested/deep/ok.avi"), MIN_SIZE);
        write_file(&dir.join("nested/ignore.webm.part"), MIN_SIZE);

        let found = scan(&[dir.to_string_lossy().to_string()]);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.contains(&"ok.mp4".to_string()));
        assert!(names.contains(&"exact.MKV".to_string()));
        assert!(names.contains(&"ok.avi".to_string()));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dedupes_across_and_within_folders() {
        let dir = temp_dir("dedupe");
        write_file(&dir.join("a.mp4"), MIN_SIZE);
        write_file(&dir.join("sub/a.mp4"), MIN_SIZE);
        let dir_str = dir.to_string_lossy().to_string();
        let found = scan(&[dir_str.clone(), dir_str.clone()]);
        assert_eq!(found.len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn counts_and_missing() {
        let dir = temp_dir("count");
        write_file(&dir.join("a.mp4"), MIN_SIZE);
        write_file(&dir.join("b.mkv"), MIN_SIZE);
        assert_eq!(count(&dir.to_string_lossy()).unwrap(), 2);
        assert_eq!(count("/definitely/not/here").unwrap_err(), "not found");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn handles_symlink_loop() {
        let dir = temp_dir("loop");
        write_file(&dir.join("a.mp4"), MIN_SIZE);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir, dir.join("self")).unwrap();
        let found = scan(&[dir.to_string_lossy().to_string()]);
        assert_eq!(found.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
