//! Deterministic, sanitized logical-corpus metadata.

use blake3::Hasher;
use serde::{Deserialize, Serialize};

/// Provider allocation used for a generated corpus.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProviderMix {
    Even,
}

/// Corpus sizing contract. `logical_bytes` intentionally permits sparse
/// padding; `physical_bytes` is always reported separately.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CorpusSpec {
    pub seed: u64,
    pub logical_bytes: u64,
    pub sources: usize,
    pub giant_eof_source_bytes: u64,
    pub provider_mix: ProviderMix,
}

impl CorpusSpec {
    #[must_use]
    pub const fn release() -> Self {
        Self {
            seed: 0xA6_B0_02,
            logical_bytes: 5 * 1024 * 1024 * 1024,
            sources: 2_560,
            giant_eof_source_bytes: 838 * 1024 * 1024,
            provider_mix: ProviderMix::Even,
        }
    }
}

/// One source declaration, deliberately containing no native transcript text
/// or local path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CorpusSource {
    pub source_ordinal: u32,
    pub provider: String,
    pub policy: String,
    pub logical_bytes: u64,
}

/// Hashable manifest passed between a gate runner and cutover verifier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CorpusManifest {
    pub seed: u64,
    pub logical_bytes: u64,
    pub physical_bytes: u64,
    pub sources: Vec<CorpusSource>,
    pub hash: String,
}

/// Builds the deterministic metadata manifest without materializing logical
/// sparse padding or retaining any source content.
#[must_use]
pub fn manifest(spec: &CorpusSpec) -> CorpusManifest {
    let mut sources = Vec::with_capacity(spec.sources);
    let source_count = u64::try_from(spec.sources).unwrap_or(u64::MAX).max(1);
    let normal_logical = spec
        .logical_bytes
        .saturating_sub(spec.giant_eof_source_bytes)
        / source_count;
    for ordinal in 0..spec.sources {
        let is_giant_eof = ordinal == 0;
        sources.push(CorpusSource {
            source_ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
            provider: if ordinal % 2 == 0 { "claude" } else { "codex" }.into(),
            policy: if is_giant_eof {
                "undated_eof"
            } else {
                "trusted_replay"
            }
            .into(),
            logical_bytes: if is_giant_eof {
                spec.giant_eof_source_bytes
            } else {
                normal_logical
            },
        });
    }
    let physical_bytes = u64::try_from(sources.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(256);
    let mut manifest = CorpusManifest {
        seed: spec.seed,
        logical_bytes: spec.logical_bytes,
        physical_bytes,
        sources,
        hash: String::new(),
    };
    manifest.hash = manifest_hash(&manifest);
    manifest
}

/// Returns the stable hash of all content-free manifest fields.
#[must_use]
pub fn manifest_hash(manifest: &CorpusManifest) -> String {
    let mut hasher = Hasher::new();
    hasher.update(&manifest.seed.to_le_bytes());
    hasher.update(&manifest.logical_bytes.to_le_bytes());
    hasher.update(&manifest.physical_bytes.to_le_bytes());
    for source in &manifest.sources {
        hasher.update(&source.source_ordinal.to_le_bytes());
        hasher.update(source.provider.as_bytes());
        hasher.update(source.policy.as_bytes());
        hasher.update(&source.logical_bytes.to_le_bytes());
    }
    hasher.finalize().to_hex().to_string()
}
