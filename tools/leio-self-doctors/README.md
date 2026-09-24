# LEIO repository checks

These checks govern this source repository. They are not registered in the LEIO
engine and are not policy inherited by repositories installing the plugin.

Build and test from this repository:

```sh
cargo test --manifest-path tools/leio-self-doctors/Cargo.toml
cargo build --manifest-path tools/leio-self-doctors/Cargo.toml
leio-code --repo "$PWD" trust-doctor-pack --binary tools/leio-self-doctors/target/debug/leio-self-doctors
leio-code --repo "$PWD" doctor self-contract
leio-code --repo "$PWD" doctor leio-release-coherence
```

When using an explicit `CARGO_TARGET_DIR`, use that directory's built executable
in the trust command. Trust is repository-specific and must be renewed after
catalog or binary changes. See `docs/REPOSITORY-DOCTORS.md` for the protocol.
