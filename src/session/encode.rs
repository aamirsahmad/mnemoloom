//! cwd <-> Claude project directory name.
//!
//! Claude Code names `projects/<dir>` by replacing every non `[A-Za-z0-9]`
//! UTF-16 code unit with `-`; names longer than 200 units are cut to 200
//! and suffixed with `-<base36(|java_hash(cwd)|)>`. A narrower legacy
//! rule (`/ _ .` only) also exists on disk; that legacy form and the untruncated form are still
//! accepted when looking directories up.

use std::path::{Path, PathBuf};

/// Longest project dir name Claude Code writes before hashing.
pub const MAX_ENCODED_LEN: usize = 200;

/// Every non-alphanumeric (per UTF-16 unit) -> `-`, never truncated.
pub fn sanitize_project_path(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len());
    for c in cwd.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('-');
            }
        }
    }
    out
}

/// JS `(h << 5) - h + charCode | 0` over UTF-16 units.
fn java_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, u| h.wrapping_mul(31).wrapping_add(i32::from(u)))
}

fn base36(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push(char::from_digit((n % 36) as u32, 36).unwrap_or('0'));
        n /= 36;
    }
    digits.iter().rev().collect()
}

/// Claude Code's project dir name (what it reads and writes):
/// `/home/a.b/my_proj` -> `-home-a-b-my-proj`; > 200 chars ->
/// `<first 200>-<hash>`.
pub fn encode_project_path(cwd: &str) -> String {
    let full = sanitize_project_path(cwd);
    if full.len() <= MAX_ENCODED_LEN {
        return full;
    }
    let hash = base36(i64::from(java_hash(cwd)).unsigned_abs());
    format!("{}-{hash}", &full[..MAX_ENCODED_LEN])
}

/// Legacy encoding: only `/`, `_`, `.` become `-`.
pub fn encode_project_path_legacy(cwd: &str) -> String {
    cwd.replace(['/', '_', '.'], "-")
}

/// `<claude_home>/projects/<encode(cwd)>` (where new sessions are written).
pub fn project_dir(claude_home: &Path, cwd: &str) -> PathBuf {
    claude_home.join("projects").join(encode_project_path(cwd))
}

/// Candidate project dirs for `cwd`, deduplicated: Claude Code's name
/// first, then the untruncated and legacy forms, then (for long
/// cwds) existing dirs sharing the 200-char prefix with another hash.
pub fn project_dir_candidates(claude_home: &Path, cwd: &str) -> Vec<PathBuf> {
    let projects = claude_home.join("projects");
    let full = sanitize_project_path(cwd);
    let mut names = vec![
        encode_project_path(cwd),
        full.clone(),
        encode_project_path_legacy(cwd),
    ];
    if full.len() > MAX_ENCODED_LEN
        && let Ok(rd) = std::fs::read_dir(&projects)
    {
        let prefix = format!("{}-", &full[..MAX_ENCODED_LEN]);
        let mut extra: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&prefix))
            .collect();
        extra.sort();
        names.extend(extra);
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for n in names {
        let p = projects.join(n);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Whether a project dir name could encode `cwd` (Claude Code, untruncated,
/// legacy, or the 200-char prefix with any hash suffix).
pub fn dir_matches_cwd(dir_name: &str, cwd: &str) -> bool {
    let full = sanitize_project_path(cwd);
    dir_name == encode_project_path(cwd)
        || dir_name == full
        || dir_name == encode_project_path_legacy(cwd)
        || (full.len() > MAX_ENCODED_LEN
            && dir_name.starts_with(&format!("{}-", &full[..MAX_ENCODED_LEN])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broad_encoding() {
        assert_eq!(
            encode_project_path("/home/alice.smith/src/my_proj"),
            "-home-alice-smith-src-my-proj"
        );
        assert_eq!(encode_project_path("/a b/c+d@e"), "-a-b-c-d-e");
        // Non-BMP char = two UTF-16 units = two dashes (matches JS replace).
        assert_eq!(encode_project_path("/x\u{1F600}"), "-x--");
        assert_eq!(encode_project_path("/é"), "--");
    }

    #[test]
    fn long_paths_truncate_and_hash() {
        // Same algorithm as Claude Code 2.1.293 (cross-checked against an
        // independent JS-style hashCode + base36 implementation).
        let cwd = format!("/home/xy/{}", "very_long_directory_name/".repeat(10));
        let enc = encode_project_path(&cwd);
        assert_eq!(enc.len(), 207);
        assert!(enc.ends_with("y-long-direct-totn1v"), "{enc}");
        assert_eq!(&enc[..200], &sanitize_project_path(&cwd)[..200]);
        // Exactly 200 is not hashed.
        let c200 = format!("/{}", "a".repeat(199));
        assert_eq!(encode_project_path(&c200), sanitize_project_path(&c200));

        let d = tempfile::tempdir().unwrap();
        let other_hash = format!("{}-zzz", &sanitize_project_path(&cwd)[..200]);
        std::fs::create_dir_all(d.path().join("projects").join(&other_hash)).unwrap();
        let c = project_dir_candidates(d.path(), &cwd);
        assert_eq!(c[0], d.path().join("projects").join(&enc));
        assert!(c.contains(&d.path().join("projects").join(sanitize_project_path(&cwd))));
        assert!(c.contains(&d.path().join("projects").join(&other_hash)));
        assert!(dir_matches_cwd(&other_hash, &cwd));
        assert!(dir_matches_cwd(&enc, &cwd));
    }

    #[test]
    fn legacy_encoding_and_candidates() {
        assert_eq!(encode_project_path_legacy("/a b/c_d.e"), "-a b-c-d-e");
        let c = project_dir_candidates(Path::new("/h"), "/a b/c");
        assert_eq!(
            c,
            vec![
                PathBuf::from("/h/projects/-a-b-c"),
                PathBuf::from("/h/projects/-a b-c")
            ]
        );
        let same = project_dir_candidates(Path::new("/h"), "/a/b_c");
        assert_eq!(same.len(), 1);
        assert!(dir_matches_cwd("-a b-c", "/a b/c"));
        assert!(dir_matches_cwd("-a-b-c", "/a b/c"));
        assert_eq!(
            project_dir(Path::new("/h"), "/a.b"),
            PathBuf::from("/h/projects/-a-b")
        );
    }
}
