use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, TokenData, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ─────────────────────────────────────────────────────────────
// Password hashing
// ─────────────────────────────────────────────────────────────

pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2.hash_password(password.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool, argon2::password_hash::Error> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

// ─────────────────────────────────────────────────────────────
// JWT
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String, // user_id
    pub exp: usize,
    pub iat: usize,
    pub token_type: String, // "access" or "refresh"
}

pub fn create_access_token(
    user_id: &Uuid,
    secret: &str,
) -> Result<String, jsonwebtoken::errors::Error> {
    let now = Utc::now();
    let claims = Claims {
        sub: user_id.to_string(),
        iat: now.timestamp() as usize,
        exp: (now + Duration::hours(1)).timestamp() as usize,
        token_type: "access".to_string(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

pub fn create_refresh_token(
    user_id: &Uuid,
    secret: &str,
) -> Result<String, jsonwebtoken::errors::Error> {
    let now = Utc::now();
    let claims = Claims {
        sub: user_id.to_string(),
        iat: now.timestamp() as usize,
        exp: (now + Duration::days(7)).timestamp() as usize,
        token_type: "refresh".to_string(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

pub fn validate_token(
    token: &str,
    secret: &str,
) -> Result<TokenData<Claims>, jsonwebtoken::errors::Error> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "test-secret-key-for-unit-tests-only-32chars!";

    // ── Password Hashing ────────────────────────────────────
    #[test]
    fn test_hash_password_returns_valid_hash() {
        let hash = hash_password("password123").unwrap();
        assert!(hash.starts_with("$argon2"));
        assert!(hash.len() > 50);
    }

    #[test]
    fn test_hash_password_different_salt_each_time() {
        let h1 = hash_password("same_pass").unwrap();
        let h2 = hash_password("same_pass").unwrap();
        assert_ne!(h1, h2); // Different salt → different hash
    }

    #[test]
    fn test_verify_password_correct() {
        let hash = hash_password("correct_password").unwrap();
        assert!(verify_password("correct_password", &hash).unwrap());
    }

    #[test]
    fn test_verify_password_wrong() {
        let hash = hash_password("correct_password").unwrap();
        assert!(!verify_password("wrong_password", &hash).unwrap());
    }

    #[test]
    fn test_verify_password_empty() {
        let hash = hash_password("something").unwrap();
        assert!(!verify_password("", &hash).unwrap());
    }

    #[test]
    fn test_hash_empty_password() {
        let hash = hash_password("").unwrap();
        assert!(verify_password("", &hash).unwrap());
    }

    // ── JWT ─────────────────────────────────────────────────
    #[test]
    fn test_create_and_validate_access_token() {
        let user_id = Uuid::new_v4();
        let token = create_access_token(&user_id, TEST_SECRET).unwrap();
        assert!(!token.is_empty());

        let data = validate_token(&token, TEST_SECRET).unwrap();
        assert_eq!(data.claims.sub, user_id.to_string());
        assert_eq!(data.claims.token_type, "access");
    }

    #[test]
    fn test_create_and_validate_refresh_token() {
        let user_id = Uuid::new_v4();
        let token = create_refresh_token(&user_id, TEST_SECRET).unwrap();
        assert!(!token.is_empty());

        let data = validate_token(&token, TEST_SECRET).unwrap();
        assert_eq!(data.claims.sub, user_id.to_string());
        assert_eq!(data.claims.token_type, "refresh");
    }

    #[test]
    fn test_validate_token_wrong_secret() {
        let user_id = Uuid::new_v4();
        let token = create_access_token(&user_id, TEST_SECRET).unwrap();
        let result = validate_token(&token, "wrong-secret");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_token_garbage_input() {
        let result = validate_token("not.a.valid.jwt", TEST_SECRET);
        assert!(result.is_err());
    }

    #[test]
    fn test_access_token_claims_have_valid_timestamps() {
        let user_id = Uuid::new_v4();
        let token = create_access_token(&user_id, TEST_SECRET).unwrap();
        let data = validate_token(&token, TEST_SECRET).unwrap();

        assert!(data.claims.iat > 0);
        assert!(data.claims.exp > data.claims.iat);
        // Access token expires in ~1 hour
        let diff = data.claims.exp - data.claims.iat;
        assert_eq!(diff, 3600);
    }

    #[test]
    fn test_refresh_token_claims_have_valid_timestamps() {
        let user_id = Uuid::new_v4();
        let token = create_refresh_token(&user_id, TEST_SECRET).unwrap();
        let data = validate_token(&token, TEST_SECRET).unwrap();

        // Refresh token expires in ~7 days
        let diff = data.claims.exp - data.claims.iat;
        assert_eq!(diff, 7 * 24 * 3600);
    }

    #[test]
    fn test_expired_token_rejected() {
        let user_id = Uuid::new_v4();
        let now = Utc::now();
        let claims = Claims {
            sub: user_id.to_string(),
            iat: (now - Duration::hours(2)).timestamp() as usize,
            exp: (now - Duration::hours(1)).timestamp() as usize, // expired 1hr ago
            token_type: "access".to_string(),
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(TEST_SECRET.as_bytes()),
        )
        .unwrap();

        let result = validate_token(&token, TEST_SECRET);
        assert!(result.is_err());
    }

    #[test]
    fn test_different_users_get_different_tokens() {
        let u1 = Uuid::new_v4();
        let u2 = Uuid::new_v4();
        let t1 = create_access_token(&u1, TEST_SECRET).unwrap();
        let t2 = create_access_token(&u2, TEST_SECRET).unwrap();
        assert_ne!(t1, t2);
    }
}
