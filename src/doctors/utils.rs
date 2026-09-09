use std::fs;
use std::path::Path;
use std::process::Command;

use time::OffsetDateTime;

pub fn read_text(path: &Path, warnings: &mut Vec<String>) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(err) => {
            warnings.push(format!("failed to read {}: {}", path.display(), err));
            None
        }
    }
}

pub fn find_line(content: &str, needle: &str) -> Option<usize> {
    content
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(needle))
        .map(|(index, _)| index + 1)
}

pub fn query_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        OffsetDateTime::now_utc().unix_timestamp_nanos()
    )
}

/// Enumerate git-tracked files relative to `root`, normalized to forward slashes.
///
/// Runs `git -C <root> ls-files -z` and NUL-splits the output. Returns `None`
/// when git is unavailable, `root` is not a git repository, or the command
/// fails, so callers can fail open rather than panic in sandboxed or
/// shallow-checkout environments.
///
/// # Examples
///
/// ```ignore
/// if let Some(files) = git_tracked_files(repo_root) {
///     for f in files.iter().filter(|f| f.ends_with(".duckdb")) {
///         // inspect tracked DuckDB artifacts
///     }
/// }
/// ```
pub(crate) fn git_tracked_files(root: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let files = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).replace('\\', "/"))
        .collect();
    Some(files)
}
