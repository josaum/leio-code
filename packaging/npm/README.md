# leio-code

Prebuilt Leio Code and Leio Harness. Installing this package downloads the release binaries. It does not compile Rust.

```bash
npm install -g leio-code
leio-code --repo /absolute/path/to/repo context "fix the payment validation"
leio-harness workflow --repo /absolute/path/to/repo --dir /tmp/my-run --action show
```

Release binaries cover macOS arm64, macOS x64, and Linux x64.
