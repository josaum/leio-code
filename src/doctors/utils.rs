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
pub fn git_tracked_files(root: &Path) -> Option<Vec<String>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run_git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed with {status}");
    }

    #[test]
    fn read_text_returns_content_without_warning() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("doctor.toml");
        fs::write(&path, "kind = \"content\"\n").expect("write fixture");
        let mut warnings = Vec::new();

        let content = read_text(&path, &mut warnings);

        assert_eq!(content.as_deref(), Some("kind = \"content\"\n"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_text_failure_records_path_and_cause() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("missing.toml");
        let mut warnings = Vec::new();

        let content = read_text(&path, &mut warnings);

        assert!(content.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains(&format!("failed to read {}", path.display())));
        assert!(warnings[0].contains("No such file") || warnings[0].contains("not found"));
    }

    #[test]
    fn find_line_returns_first_one_based_match() {
        let content = "alpha\nneedle first\nneedle second\n";

        assert_eq!(find_line(content, "needle"), Some(2));
        assert_eq!(find_line(content, "absent"), None);
    }

    #[test]
    fn query_id_keeps_prefix_and_uses_a_numeric_timestamp() {
        let id = query_id("doctor-ci");
        let timestamp = id
            .strip_prefix("doctor-ci-")
            .expect("query id prefix")
            .parse::<i128>()
            .expect("nanosecond timestamp");

        assert!(timestamp > 0);
    }

    #[test]
    fn git_tracked_files_returns_none_outside_a_repository() {
        let tmp = TempDir::new().expect("tempdir");

        assert!(git_tracked_files(tmp.path()).is_none());
    }

    #[test]
    fn git_tracked_files_returns_staged_files_with_normalized_separators() {
        let tmp = TempDir::new().expect("tempdir");
        run_git(tmp.path(), &["init", "--quiet"]);
        fs::create_dir(tmp.path().join("nested")).expect("create nested directory");
        fs::write(tmp.path().join("nested/tracked.txt"), "tracked\n").expect("write tracked");
        fs::write(tmp.path().join("back\\slash.txt"), "tracked\n")
            .expect("write backslash fixture");
        fs::write(tmp.path().join("untracked.txt"), "untracked\n").expect("write untracked");
        run_git(
            tmp.path(),
            &["add", "nested/tracked.txt", "back\\slash.txt"],
        );

        let files = git_tracked_files(tmp.path()).expect("tracked files");

        assert_eq!(files, vec!["back/slash.txt", "nested/tracked.txt"]);
    }
}
