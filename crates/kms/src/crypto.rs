use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    AeadCore, Aes256Gcm, Nonce,
};
use anyhow::Result;

/// Encrypt a private key using AES-256-GCM.
/// Returns (ciphertext, nonce) — both needed for decryption.
pub fn encrypt_key(master_key: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let cipher = Aes256Gcm::new_from_slice(master_key)
        .map_err(|e| anyhow::anyhow!("Failed to create cipher: {e}"))?;

    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

    Ok((ciphertext, nonce.to_vec()))
}

/// Decrypt a private key using AES-256-GCM.
/// The key is only held in memory during this call.
pub fn decrypt_key(
    master_key: &[u8; 32],
    ciphertext: &[u8],
    nonce_bytes: &[u8],
) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(master_key)
        .map_err(|e| anyhow::anyhow!("Failed to create cipher: {e}"))?;

    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("Decryption failed — possible key mismatch: {e}"))?;

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [0x42u8; 32];
        let secret = b"my_secret_private_key_material!!";

        let (ciphertext, nonce) = encrypt_key(&key, secret).unwrap();
        assert_ne!(&ciphertext, secret);

        let decrypted = decrypt_key(&key, &ciphertext, &nonce).unwrap();
        assert_eq!(&decrypted, secret);
    }

    #[test]
    fn test_wrong_key_fails() {
        let key = [0x42u8; 32];
        let wrong_key = [0x43u8; 32];
        let secret = b"another_secret";

        let (ciphertext, nonce) = encrypt_key(&key, secret).unwrap();
        let result = decrypt_key(&wrong_key, &ciphertext, &nonce);
        assert!(result.is_err());
    }

    #[test]
    fn test_encrypt_empty_plaintext() {
        let key = [0x42u8; 32];
        let (ciphertext, nonce) = encrypt_key(&key, b"").unwrap();
        // AES-GCM produces tag even for empty plaintext
        assert!(!ciphertext.is_empty());
        let decrypted = decrypt_key(&key, &ciphertext, &nonce).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn test_encrypt_large_data() {
        let key = [0xAB; 32];
        let large_data = vec![0xFFu8; 10_000];
        let (ciphertext, nonce) = encrypt_key(&key, &large_data).unwrap();
        let decrypted = decrypt_key(&key, &ciphertext, &nonce).unwrap();
        assert_eq!(decrypted, large_data);
    }

    #[test]
    fn test_nonce_is_12_bytes() {
        let key = [0x42u8; 32];
        let (_, nonce) = encrypt_key(&key, b"data").unwrap();
        assert_eq!(nonce.len(), 12); // AES-256-GCM nonce = 96 bits
    }

    #[test]
    fn test_different_encryptions_produce_different_nonces() {
        let key = [0x42u8; 32];
        let (_, n1) = encrypt_key(&key, b"same").unwrap();
        let (_, n2) = encrypt_key(&key, b"same").unwrap();
        // Random nonces should differ (probability of collision negligible)
        assert_ne!(n1, n2);
    }

    #[test]
    fn test_different_encryptions_produce_different_ciphertext() {
        let key = [0x42u8; 32];
        let (ct1, _) = encrypt_key(&key, b"same").unwrap();
        let (ct2, _) = encrypt_key(&key, b"same").unwrap();
        assert_ne!(ct1, ct2);
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let key = [0x42u8; 32];
        let (mut ciphertext, nonce) = encrypt_key(&key, b"secret data").unwrap();
        // Flip a bit
        if let Some(byte) = ciphertext.first_mut() {
            *byte ^= 0x01;
        }
        assert!(decrypt_key(&key, &ciphertext, &nonce).is_err());
    }

    #[test]
    fn test_wrong_nonce_fails() {
        let key = [0x42u8; 32];
        let (ciphertext, _) = encrypt_key(&key, b"secret").unwrap();
        let wrong_nonce = vec![0u8; 12];
        // Highly unlikely to match random nonce
        assert!(decrypt_key(&key, &ciphertext, &wrong_nonce).is_err());
    }
}
