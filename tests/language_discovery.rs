//! Exercise discovery and classification through the public index builder.
//! Parser support alone must not hide source files from repository navigation.

use std::fs;

use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::model::{RepoIndex, SourceLanguage};

#[test]
fn supported_source_extensions_survive_discovery_and_persistence() {
    use SourceLanguage::*;

    let fixtures = [
        ("go", Go, "package sample\nfunc Example() {}\n"),
        ("c", C, "void example(void) {}\n"),
        ("h", C, "void example(void);\n"),
        ("cc", Cpp, "void example() {}\n"),
        ("cpp", Cpp, "void example() {}\n"),
        ("cxx", Cpp, "void example() {}\n"),
        ("hpp", Cpp, "class Example {};\n"),
        ("hh", Cpp, "class Example {};\n"),
        ("hxx", Cpp, "class Example {};\n"),
        ("sh", Bash, "example() { echo ready; }\n"),
        ("bash", Bash, "example() { echo ready; }\n"),
        ("java", Java, "class Example { void run() {} }\n"),
        ("kt", Kotlin, "fun example() {}\n"),
        ("kts", Kotlin, "fun example() {}\n"),
        ("html", Html, "<main id=\"example\">Ready</main>\n"),
        ("htm", Html, "<main id=\"example\">Ready</main>\n"),
        ("css", Css, ".example { color: blue; }\n"),
        ("cs", CSharp, "class Example { void Run() {} }\n"),
        ("csx", CSharp, "System.Console.WriteLine(\"ready\");\n"),
        ("razor", Razor, "<p>Ready</p>\n@code { void Run() {} }\n"),
        (
            "cshtml",
            Razor,
            "<p>Ready</p>\n@functions { void Run() {} }\n",
        ),
        ("rs", Rust, "fn example() {}\n"),
        ("py", Python, "def example():\n    pass\n"),
        ("ts", TypeScript, "export function example(): void {}\n"),
        (
            "tsx",
            Tsx,
            "export function Example() { return <p>Ready</p>; }\n",
        ),
        ("js", JavaScript, "export function example() {}\n"),
        ("mjs", JavaScript, "export function example() {}\n"),
        ("swift", Swift, "func example() {}\n"),
        ("sql", Sql, "CREATE TABLE example (id INTEGER);\n"),
    ];
    let repo = tempfile::tempdir().expect("temporary repository");
    fs::create_dir(repo.path().join("src")).expect("source directory");
    for (extension, _, source) in fixtures {
        fs::write(repo.path().join(format!("src/example.{extension}")), source)
            .expect("source fixture");
    }
    let index_path = default_index_path(repo.path());
    build_or_update_index(repo.path(), &index_path, true).expect("fresh index");

    // Inspect the persisted representation consumed by later CLI/MCP requests.
    let persisted: RepoIndex =
        serde_json::from_slice(&fs::read(&index_path).expect("persisted index"))
            .expect("readable persisted index");
    let incremental =
        build_or_update_index(repo.path(), &index_path, false).expect("incremental index");
    for (phase, index) in [("persisted", persisted), ("incremental", incremental)] {
        let mut failures = Vec::new();
        for (extension, expected, _) in fixtures {
            let path = format!("src/example.{extension}");
            match index.files.iter().find(|file| file.path == path) {
                None => failures.push(format!("{path}: missing")),
                Some(file) if file.language != expected => failures.push(format!(
                    "{path}: expected {expected:?}, found {:?}",
                    file.language
                )),
                Some(_) => {}
            }
        }
        assert!(
            failures.is_empty(),
            "{phase} source coverage gaps:\n{}",
            failures.join("\n")
        );
    }
}
