//! Targeted parsers for first-party package versions.

use serde_json::Value as JsonValue;

use super::utils::find_line;

#[derive(Clone, Copy)]
pub(super) enum VersionKind<'a> {
    JsonPackage,
    NpmPackageLockRoot,
    CargoPackage,
    CargoLockPackage(&'a str),
}

pub(super) struct VersionManifest<'a> {
    pub path: &'a str,
    pub kind: VersionKind<'a>,
    pub label: &'a str,
}

pub(super) fn json_package_version(source: &str) -> Option<String> {
    serde_json::from_str::<JsonValue>(source)
        .ok()?
        .get("version")?
        .as_str()
        .map(str::to_string)
}

pub(super) fn cargo_package_version(source: &str) -> Option<String> {
    toml::from_str::<toml::Value>(source)
        .ok()?
        .get("package")?
        .get("version")?
        .as_str()
        .map(str::to_string)
}

pub(super) fn npm_lock_root_version(source: &str) -> Option<String> {
    let value = serde_json::from_str::<JsonValue>(source).ok()?;
    value
        .pointer("/packages//version")
        .and_then(JsonValue::as_str)
        .or_else(|| value.get("version").and_then(JsonValue::as_str))
        .map(str::to_string)
}

pub(super) fn cargo_lock_package_version(source: &str, package_name: &str) -> Option<String> {
    toml::from_str::<toml::Value>(source)
        .ok()?
        .get("package")?
        .as_array()?
        .iter()
        .find(|package| package.get("name").and_then(toml::Value::as_str) == Some(package_name))?
        .get("version")?
        .as_str()
        .map(str::to_string)
}

pub(super) fn version_from_source(source: &str, kind: VersionKind<'_>) -> Option<String> {
    match kind {
        VersionKind::JsonPackage => json_package_version(source),
        VersionKind::NpmPackageLockRoot => npm_lock_root_version(source),
        VersionKind::CargoPackage => cargo_package_version(source),
        VersionKind::CargoLockPackage(package_name) => {
            cargo_lock_package_version(source, package_name)
        }
    }
}

pub(super) fn version_line(source: &str, kind: VersionKind<'_>) -> Option<usize> {
    match kind {
        VersionKind::CargoLockPackage(package_name) => {
            cargo_lock_package_version_line(source, package_name)
        }
        VersionKind::CargoPackage => find_line(source, "version ="),
        VersionKind::JsonPackage | VersionKind::NpmPackageLockRoot => {
            find_line(source, "\"version\"")
        }
    }
}

fn cargo_lock_package_version_line(source: &str, package_name: &str) -> Option<usize> {
    let name_line = format!("name = \"{package_name}\"");
    let mut in_package = false;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            in_package = false;
        }
        if trimmed == name_line {
            in_package = true;
        } else if in_package && trimmed.starts_with("version =") {
            return Some(index + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_lock_selects_only_the_named_package() {
        let lock = r#"
[[package]]
name = "router"
version = "2.2.0"

[[package]]
name = "leio-code"
version = "2.3.0"
"#;
        assert_eq!(
            cargo_lock_package_version(lock, "leio-code").as_deref(),
            Some("2.3.0")
        );
    }

    #[test]
    fn npm_lock_reads_only_the_root_package() {
        let lock = r#"{
  "version": "2.3.0",
  "packages": {
    "": {"name": "@example/leio-code-mcp", "version": "2.3.0"},
    "node_modules/router": {"version": "2.2.0"}
  }
}"#;
        assert_eq!(npm_lock_root_version(lock).as_deref(), Some("2.3.0"));
    }

    #[test]
    fn json_and_cargo_manifests_read_only_package_versions() {
        let json = r#"{"name":"leio-code","version":"2.3.0","dependency":{"version":"9.9.9"}}"#;
        assert_eq!(json_package_version(json).as_deref(), Some("2.3.0"));

        let cargo = r#"
[package]
name = "leio-code"
version = "2.3.0"

[dependencies]
router = "2.2.0"
"#;
        assert_eq!(cargo_package_version(cargo).as_deref(), Some("2.3.0"));
    }

    #[test]
    fn cargo_lock_version_line_is_scoped_to_named_package() {
        let lock = "[[package]]\nname = \"router\"\nversion = \"2.2.0\"\n\n[[package]]\nname = \"leio-code\"\nversion = \"2.3.0\"\n";
        assert_eq!(
            version_line(lock, VersionKind::CargoLockPackage("leio-code")),
            Some(7)
        );
    }
}
