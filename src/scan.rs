use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::ID_FILE;

/// Folders never worth descending into: build output, package libraries, VCS.
fn skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with(".Rcheck")
        || name.ends_with("_files")
        || matches!(name, "node_modules" | "target" | "renv" | "_freeze" | "site_libs" | "__pycache__")
}

/// Every `.ptrack-id` under `root` up to `max_depth` levels down, keyed by the id
/// it holds. Symlinks are not followed.
pub fn find_id_files(root: &Path, max_depth: usize) -> BTreeMap<String, Vec<PathBuf>> {
    let mut found: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_file() && name == ID_FILE {
                if let Ok(id) = fs::read_to_string(e.path()) {
                    found.entry(id.trim().to_string()).or_default().push(dir.clone());
                }
            } else if ft.is_dir() && depth < max_depth && !skip_dir(&name) {
                stack.push((e.path(), depth + 1));
            }
        }
    }
    for dirs in found.values_mut() {
        dirs.sort();
    }
    found
}
