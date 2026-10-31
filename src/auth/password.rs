//! Password hashing (R-C1): argon2id with a random salt per password. Verification is constant
//! time in the hash; a user that does not exist is verified against a dummy hash so the answer
//! takes about as long.

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};

pub fn hash(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| "could not hash the password".to_string())
}

pub fn verify(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

/// Spend the time of a verification when there is nothing to verify against.
pub fn burn(password: &str) {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let d = DUMMY.get_or_init(|| hash("dummy password for timing").unwrap_or_default());
    let _ = verify(password, d);
}
