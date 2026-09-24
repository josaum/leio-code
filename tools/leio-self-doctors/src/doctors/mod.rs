pub use leio_code::doctors::{Doctor, utils, version_manifest};
pub mod leio_release_coherence;
pub mod self_contract;
pub fn registry() -> Vec<Box<dyn Doctor>> {
    vec![
        Box::new(self_contract::SelfContractDoctor),
        Box::new(leio_release_coherence::LeioReleaseCoherenceDoctor),
    ]
}
