use crate::seed::Seed;
use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Prefix distinguishing encrypted state rows from legacy plaintext JSON.
pub(crate) const STATE_CIPHER_PREFIX: &str = "aead:v1:";
const STATE_CIPHER_AAD: &[u8] = b"outpost-swap-state-v1";
const NONCE_LEN: usize = 12;
const KEY_CHECK_PLAINTEXT: &[u8] = b"outpost-swap-state-key-v1";

/// Encrypts the serialized swap state stored in SQLite. The key is derived
/// from the 64-byte wallet seed, which is available to both the GUI and ASB
/// after unlock. Metadata rows (swap IDs, timestamps, addresses) remain
/// plaintext; active swap secrets do not.
#[derive(Clone)]
pub struct StateCipher {
    key: [u8; 32],
}

impl StateCipher {
    pub fn from_seed(seed: &Seed) -> Self {
        Self::from_key(seed.derive_state_encryption_key())
    }

    pub fn from_key(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn key_id(&self) -> String {
        hex::encode(Sha256::digest(self.key))
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<String> {
        let cipher = ChaCha20Poly1305::new_from_slice(&self.key)
            .expect("StateCipher always has a 32-byte key");
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad: STATE_CIPHER_AAD,
                },
            )
            .map_err(|_| anyhow!("Failed to encrypt swap state"))?;

        let mut packed = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        packed.extend_from_slice(&nonce_bytes);
        packed.extend_from_slice(&ciphertext);
        Ok(format!(
            "{}{}",
            STATE_CIPHER_PREFIX,
            BASE64_STANDARD.encode(packed)
        ))
    }

    /// Decrypts a state row, accepting legacy plaintext JSON during migration.
    pub fn decrypt(&self, stored: &str) -> Result<Vec<u8>> {
        let Some(encoded) = stored.strip_prefix(STATE_CIPHER_PREFIX) else {
            return Ok(stored.as_bytes().to_vec());
        };
        let packed = BASE64_STANDARD
            .decode(encoded)
            .context("Failed to decode encrypted swap state")?;
        if packed.len() <= NONCE_LEN {
            bail!("Encrypted swap state is too short");
        }
        let (nonce_bytes, ciphertext) = packed.split_at(NONCE_LEN);
        let cipher = ChaCha20Poly1305::new_from_slice(&self.key)
            .expect("StateCipher always has a 32-byte key");
        cipher
            .decrypt(
                Nonce::from_slice(nonce_bytes),
                Payload {
                    msg: ciphertext,
                    aad: STATE_CIPHER_AAD,
                },
            )
            .map_err(|_| anyhow!("Failed to decrypt swap state (wrong seed or tampered data)"))
    }
}

pub(crate) fn key_check_plaintext() -> &'static [u8] {
    KEY_CHECK_PLAINTEXT
}

/// True when the decrypted key-check row holds the expected text, i.e. the seed is the right one.
pub(crate) fn is_key_check_plaintext(plaintext: &[u8]) -> bool {
    plaintext == KEY_CHECK_PLAINTEXT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_cipher_round_trips_and_marks_ciphertext() {
        let cipher = StateCipher::from_key([7u8; 32]);
        let ciphertext = cipher.encrypt(b"{\"secret\":true}").unwrap();

        assert!(ciphertext.starts_with(STATE_CIPHER_PREFIX));
        assert_eq!(cipher.decrypt(&ciphertext).unwrap(), b"{\"secret\":true}");
        assert_eq!(
            cipher.decrypt("{\"legacy\":true}").unwrap(),
            b"{\"legacy\":true}"
        );
    }

    #[test]
    fn state_cipher_rejects_tampering_and_wrong_keys() {
        let cipher = StateCipher::from_key([7u8; 32]);
        let ciphertext = cipher.encrypt(b"state").unwrap();
        let mut tampered = ciphertext.into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).unwrap();

        if let Ok(plaintext) = cipher.decrypt(&tampered) {
            assert_ne!(plaintext, b"state");
        }

        let wrong = StateCipher::from_key([8u8; 32]);
        assert!(wrong.decrypt(&cipher.encrypt(b"state").unwrap()).is_err());
    }

    #[test]
    fn the_key_check_accepts_only_its_own_text() {
        assert!(is_key_check_plaintext(key_check_plaintext()));
        assert!(!is_key_check_plaintext(b"something else"));
    }
}
