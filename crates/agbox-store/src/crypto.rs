#[cfg(feature = "test-support")]
use std::sync::Mutex;

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, Generate, KeyInit, Payload},
};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("credential store failure: {0}")]
    Credential(String),
    #[error("cipher operation failed")]
    Cipher,
    #[error("master key must be exactly 32 bytes")]
    KeyLength,
}

pub trait KeyProvider: Send + Sync {
    /// Loads the process-wide evidence-encryption key.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError`] when the credential store cannot provide a
    /// valid 32-byte key.
    fn master_key(&self) -> Result<Zeroizing<[u8; 32]>, CryptoError>;
}

#[cfg(feature = "test-support")]
pub struct MemoryKeyProvider(Mutex<[u8; 32]>);

#[cfg(feature = "test-support")]
impl std::fmt::Debug for MemoryKeyProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryKeyProvider")
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "test-support")]
impl MemoryKeyProvider {
    #[must_use]
    pub fn fixed(key: [u8; 32]) -> Self {
        Self(Mutex::new(key))
    }
}

#[cfg(feature = "test-support")]
impl KeyProvider for MemoryKeyProvider {
    fn master_key(&self) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
        self.0
            .lock()
            .map(|key| Zeroizing::new(*key))
            .map_err(|error| CryptoError::Credential(error.to_string()))
    }
}

#[derive(Debug, Default)]
pub struct KeyringKeyProvider;

impl KeyProvider for KeyringKeyProvider {
    fn master_key(&self) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
        let entry = keyring::Entry::new("com.agbox.runtime.v2", "state-master-key")
            .map_err(|error| CryptoError::Credential(error.to_string()))?;
        match entry.get_secret() {
            Ok(secret) => {
                let secret = Zeroizing::new(secret);
                if secret.len() != 32 {
                    return Err(CryptoError::KeyLength);
                }
                let mut key = Zeroizing::new([0_u8; 32]);
                key.copy_from_slice(&secret);
                Ok(key)
            }
            Err(keyring::Error::NoEntry) => {
                let mut generated = chacha20poly1305::aead::Key::<XChaCha20Poly1305>::generate();
                let mut result = Zeroizing::new([0_u8; 32]);
                result.copy_from_slice(generated.as_slice());
                let set_result = entry.set_secret(result.as_slice());
                generated.as_mut_slice().fill(0);
                set_result.map_err(|error| CryptoError::Credential(error.to_string()))?;
                Ok(result)
            }
            Err(error) => Err(CryptoError::Credential(error.to_string())),
        }
    }
}

/// Seals plaintext in the versioned authenticated-encryption envelope.
///
/// # Errors
///
/// Returns [`CryptoError::Cipher`] if encryption fails.
pub fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XNonce::generate();
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Cipher)?;
    let mut envelope = b"AGBX\x01".to_vec();
    envelope.extend_from_slice(&nonce);
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

/// Opens and authenticates a versioned encrypted envelope.
///
/// # Errors
///
/// Returns [`CryptoError::Cipher`] for malformed, unauthenticated, or
/// undecryptable envelopes.
pub fn open(key: &[u8; 32], aad: &[u8], envelope: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if envelope.len() < 5 + 24 || &envelope[..5] != b"AGBX\x01" {
        return Err(CryptoError::Cipher);
    }
    let nonce = XNonce::try_from(&envelope[5..29]).map_err(|_| CryptoError::Cipher)?;
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            &nonce,
            Payload {
                msg: &envelope[29..],
                aad,
            },
        )
        .map_err(|_| CryptoError::Cipher)
}
