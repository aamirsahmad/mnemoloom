//! Path helper shared by the filters and git.

use std::fs;
use std::path::{Path, PathBuf};

/// Canonical path, or the path itself when it cannot be resolved.
pub fn resolve(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_falls_back_to_input() {
        let missing = Path::new("/definitely/missing/dir");
        assert_eq!(resolve(missing), missing);
    }
}
