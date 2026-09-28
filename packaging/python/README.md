# leio-code

Prebuilt Leio Code and Leio Harness. Installing this package does not compile Rust.

```bash
pip install leio-code
leio-code --repo /absolute/path/to/repo context "fix the payment validation"
leio-harness workflow --repo /absolute/path/to/repo --dir /tmp/my-run --action show
```

Each command reads one repository. Pass `--repo` or, for the MCP server, `repo_root`.

Release binaries cover macOS arm64, macOS x64, and Linux x64.
