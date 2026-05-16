//! Sandbox helpers for tools that touch the filesystem.
//!
//! Every path the agent provides is resolved relative to the *current project* and refused
//! if it tries to escape. We reject absolute paths, `..` components, root components, and
//! Windows drive prefixes — anything that could land us outside the project tree.
//!
//! This is a path-shape check rather than a canonicalize-and-compare: it stays correct for
//! paths that don't exist yet (so `write_file` works the first time), and it works even
//! when the project root itself contains symlinks the OS won't dereference.

use std::path::{Component, Path, PathBuf};

use super::registry::ToolError;

/// Resolve `raw` (a user-supplied path string) against `root`, refusing anything that
/// escapes the sandbox. Returns the joined absolute path.
///
/// `raw` may be `"."`, `""`, `"./a/b/c"`, `"a/b/c"`, or `"a\\b\\c"` on Windows. It must
/// **not** be absolute, contain `..`, or start with a path separator / drive prefix.
pub fn resolve_within(root: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "." {
        return Ok(root.to_path_buf());
    }
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return Err(ToolError::PathEscape(format!(
            "absolute paths not allowed in tool args: `{raw}`"
        )));
    }
    let mut joined = root.to_path_buf();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => joined.push(part),
            Component::CurDir => {} // "." is a no-op
            Component::ParentDir => {
                return Err(ToolError::PathEscape(format!(
                    "path component `..` not allowed in tool args: `{raw}`"
                )));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(ToolError::PathEscape(format!(
                    "root / drive prefix not allowed in tool args: `{raw}`"
                )));
            }
        }
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        // Use a Windows-style root so on Windows the absolute-path check fires correctly;
        // POSIX builds reject "/foo" via the same Path::is_absolute() check below.
        if cfg!(windows) {
            PathBuf::from(r"C:\proj")
        } else {
            PathBuf::from("/proj")
        }
    }

    #[test]
    fn empty_and_dot_resolve_to_root() {
        let r = root();
        assert_eq!(resolve_within(&r, "").unwrap(), r);
        assert_eq!(resolve_within(&r, ".").unwrap(), r);
    }

    #[test]
    fn normal_paths_are_joined() {
        let r = root();
        let p = resolve_within(&r, "datasets/sft.jsonl").unwrap();
        assert!(p.ends_with("datasets/sft.jsonl") || p.ends_with(r"datasets\sft.jsonl"));
        assert!(p.starts_with(&r));
    }

    #[test]
    fn absolute_paths_are_rejected() {
        let r = root();
        if cfg!(windows) {
            assert!(resolve_within(&r, r"C:\Windows\System32").is_err());
        } else {
            assert!(resolve_within(&r, "/etc/passwd").is_err());
        }
    }

    #[test]
    fn parent_dir_escape_is_rejected() {
        let r = root();
        assert!(resolve_within(&r, "..").is_err());
        assert!(resolve_within(&r, "../etc/passwd").is_err());
        assert!(resolve_within(&r, "datasets/../../secret").is_err());
    }

    #[test]
    fn dot_segments_in_path_are_skipped() {
        let r = root();
        let p = resolve_within(&r, "./datasets/./sft.jsonl").unwrap();
        assert!(p.starts_with(&r));
        assert!(p.ends_with("sft.jsonl"));
    }
}
