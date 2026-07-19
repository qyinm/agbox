use agbox_core::{EventId, Provider, SemanticKey, SourceIdentity, WorkId};

#[test]
fn source_ids_are_retry_stable_and_generation_specific() {
    let source = SourceIdentity {
        provider: Provider::Codex,
        source_id: "src_fixture".into(),
        generation: 2,
        byte_offset: 4096,
        record_hash: "b3:deadbeef".into(),
    };

    assert_eq!(
        EventId::from_source(&source, 0),
        EventId::from_source(&source, 0)
    );
    assert_ne!(
        EventId::from_source(&source, 0),
        EventId::from_source(
            &SourceIdentity {
                generation: 3,
                ..source.clone()
            },
            0
        )
    );
    assert_ne!(WorkId::new(), WorkId::new());
    assert_eq!(
        SemanticKey::from_native(Provider::Codex, "session-a", "codex.call", "call-17"),
        SemanticKey::from_native(Provider::Codex, "session-a", "codex.call", "call-17")
    );
    assert_ne!(
        SemanticKey::from_native(Provider::Codex, "session-a", "codex.call", "call-17"),
        SemanticKey::from_native(Provider::Codex, "session-b", "codex.call", "call-17")
    );
}
