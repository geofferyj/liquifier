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
}
