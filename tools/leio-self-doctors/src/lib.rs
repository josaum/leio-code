pub use leio_code::{config, deploy_support, diagnostics, fca, jsonc, model, update};
pub mod doctors;
#[cfg(test)]
pub mod test_workspace {
    pub fn workspace_root_with(required: &str) -> Option<std::path::PathBuf> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .parent()?;
        root.join(required).exists().then(|| root.to_path_buf())
    }
}
