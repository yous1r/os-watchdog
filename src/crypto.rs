use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use argon2::{
    password_hash::{rand_core::OsRng as ArgonOsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use std::fs;
use std::path::Path;

const KEY_FILE: &str = ".os-watchdog.key";

fn get_master_key() -> Key<Aes256Gcm> {
    if Path::new(KEY_FILE).exists() {
        let key_bytes = fs::read(KEY_FILE).expect("Failed to read master key");
        if key_bytes.len() == 32 {
            return Key::<Aes256Gcm>::clone_from_slice(&key_bytes);
        }
    }
    // Generate new key
    let key = Aes256Gcm::generate_key(OsRng);
    fs::write(KEY_FILE, key.as_slice()).expect("Failed to write master key");
    key
}

pub fn encrypt_data(data: &str) -> Vec<u8> {
    let key = get_master_key();
    let cipher = Aes256Gcm::new(&key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng); // 96-bits; unique per message
    
    let mut ciphertext = cipher.encrypt(&nonce, data.as_bytes()).expect("Encryption failure!");
    
    // Prepend nonce to ciphertext so we can decrypt later
    let mut result = nonce.to_vec();
    result.append(&mut ciphertext);
    result
}

pub fn decrypt_data(data: &[u8]) -> Option<String> {
    if data.len() < 12 {
        return None;
    }
    let key = get_master_key();
    let cipher = Aes256Gcm::new(&key);
    
    let nonce = Nonce::from_slice(&data[0..12]);
    let ciphertext = &data[12..];
    
    match cipher.decrypt(nonce, ciphertext) {
        Ok(decrypted) => String::from_utf8(decrypted).ok(),
        Err(_) => None,
    }
}

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut ArgonOsRng);
    let argon2 = Argon2::default();
    argon2.hash_password(password.as_bytes(), &salt).unwrap().to_string()
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    let parsed_hash = match PasswordHash::new(hash) {
        Ok(h) => h,
        Err(_) => return false,
    };
    Argon2::default().verify_password(password.as_bytes(), &parsed_hash).is_ok()
}
