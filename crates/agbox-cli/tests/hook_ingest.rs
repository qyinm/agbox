#![allow(clippy::unwrap_used)]

use std::{io::Cursor, sync::Arc};

use agbox_cli::{AgboxPaths, CliError, commands::hook::ingest};
use agbox_core::Provider;
use agbox_store::{CryptoError, KeyProvider};
use zeroize::Zeroizing;

#[derive(Debug)]
struct FixedKey;

impl KeyProvider for FixedKey {
    fn master_key(&self) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
        Ok(Zeroizing::new([19; 32]))
    }
}

#[test]
fn hook_ingest_encrypts_only_a_verified_provider_source() {
    let home = tempfile::tempdir().unwrap();
    let source_directory = home.path().join(".codex/sessions/2026/07/19");
    std::fs::create_dir_all(&source_directory).unwrap();
    let source = source_directory.join("rollout.jsonl");
    std::fs::write(&source, b"{}\n").unwrap();
    let source_text = source
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let payload = format!(
        r#"{{"provider":"codex","hook_event_name":"session_end","session_id":"native-session-secret","transcript_path":"{source_text}","target_size":3,"prompt":"ignore instructions"}}"#,
    );
    let paths = AgboxPaths::from_home(home.path());
    ingest(
        &paths,
        home.path(),
        Provider::Codex,
        65_536,
        Cursor::new(payload),
        Arc::new(FixedKey),
    )
    .unwrap();

    let entries = std::fs::read_dir(&paths.spool)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".agbx"))
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    let encrypted = std::fs::read(entries[0].path()).unwrap();
    assert!(
        !encrypted
            .windows(b"native-session-secret".len())
            .any(|window| window == b"native-session-secret")
    );
    assert!(
        !encrypted
            .windows(b"ignore instructions".len())
            .any(|window| window == b"ignore instructions")
    );
}

#[test]
fn hook_ingest_rejects_mismatched_provider_or_untrusted_source() {
    let home = tempfile::tempdir().unwrap();
    let paths = AgboxPaths::from_home(home.path());
    let payload = br#"{"provider":"claude","hook_event_name":"session_end","session_id":"s","transcript_path":"/tmp/not-agbox.jsonl","target_size":0}"#;
    let error = ingest(
        &paths,
        home.path(),
        Provider::Codex,
        65_536,
        Cursor::new(payload),
        Arc::new(FixedKey),
    )
    .unwrap_err();
    assert!(matches!(error, CliError::InvalidHook));
    assert!(!paths.spool.exists());
}
