//! Persistent accounts and bans (a small JSON file).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::state::Level;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub nick: String,
    /// Only used by legacy SHA-256 hashes; Argon2 hashes carry their own.
    #[serde(default)]
    salt: String,
    /// Argon2id PHC string, or legacy hex SHA-256(salt + password).
    hash: String,
    #[serde(default = "legacy_scheme")]
    scheme: String,
    pub email: String,
    pub level: Level,
    pub created: u64,
    pub last_seen: u64,
    #[serde(default)]
    pub nuked: bool,
}

fn legacy_scheme() -> String {
    "sha256".into()
}

const ARGON2: &str = "argon2id";

impl Account {
    /// Hashing is deliberately slow; don't call this with the state lock held.
    pub fn new(nick: &str, password: &str, email: &str, level: Level) -> Self {
        let now = now_secs();
        Self {
            nick: nick.to_owned(),
            salt: String::new(),
            hash: argon2_hash(password),
            scheme: ARGON2.into(),
            email: email.to_owned(),
            level,
            created: now,
            last_seen: now,
            nuked: false,
        }
    }

    /// Slow (Argon2); don't call with the state lock held.
    pub fn check_password(&self, password: &str) -> bool {
        if self.scheme == ARGON2 {
            match PasswordHash::new(&self.hash) {
                Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
                Err(_) => false,
            }
        } else {
            let expected = legacy_hash(&self.salt, password);
            // constant-time compare
            expected.len() == self.hash.len()
                && expected.bytes().zip(self.hash.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
        }
    }

    /// Accounts from before v0.4 still use salted SHA-256.
    pub fn needs_rehash(&self) -> bool {
        self.scheme != ARGON2
    }

    /// Take the password hash computed (outside the lock) on a copy.
    pub fn adopt_password(&mut self, other: &Account) {
        self.salt = other.salt.clone();
        self.hash = other.hash.clone();
        self.scheme = other.scheme.clone();
    }

    /// Slow (Argon2); don't call with the state lock held.
    pub fn set_password(&mut self, password: &str) {
        self.hash = argon2_hash(password);
        self.scheme = ARGON2.into();
        self.salt.clear();
    }
}

fn argon2_hash(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(password.as_bytes(), &salt).expect("argon2 hashing").to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn legacy_hash(salt: &str, password: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(password.as_bytes());
    hex(&h.finalize())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ban {
    /// nick (lower-case) or ip address / prefix
    pub target: String,
    pub by: String,
    pub reason: String,
    pub when: u64,
}

impl Ban {
    pub fn matches(&self, nick: &str, ip: &str) -> bool {
        self.target.eq_ignore_ascii_case(nick)
            || self.target == ip
            || (self.target.ends_with('.') && ip.starts_with(&self.target))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Db {
    pub accounts: HashMap<String, Account>,
    pub bans: Vec<Ban>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Db {
    pub fn load(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let mut db = match &path {
            Some(p) if p.exists() => serde_json::from_slice(&std::fs::read(p)?)?,
            _ => Db::default(),
        };
        db.path = path;
        Ok(db)
    }

    pub fn get(&self, nick: &str) -> Option<&Account> {
        self.accounts.get(&nick.to_lowercase())
    }

    pub fn get_mut(&mut self, nick: &str) -> Option<&mut Account> {
        self.accounts.get_mut(&nick.to_lowercase())
    }

    pub fn insert(&mut self, account: Account) {
        self.accounts.insert(account.nick.to_lowercase(), account);
    }

    pub fn banned(&self, nick: &str, ip: &str) -> Option<&Ban> {
        self.bans.iter().find(|b| b.matches(nick, ip))
    }

    /// Serialized snapshot plus destination, so the write can happen
    /// outside the state lock.
    pub fn snapshot(&self) -> Option<(PathBuf, Vec<u8>)> {
        let path = self.path.clone()?;
        serde_json::to_vec_pretty(self).ok().map(|b| (path, b))
    }
}

pub fn write_snapshot(snapshot: Option<(PathBuf, Vec<u8>)>) {
    if let Some((path, bytes)) = snapshot {
        let tmp = path.with_extension("json.tmp");
        let res = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(e) = res {
            tracing::warn!("failed to save {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_hashes_still_work_and_upgrade() {
        let mut a = Account::new("old", "pw", "e", Level::User);
        a.scheme = "sha256".into();
        a.salt = "abcd".into();
        a.hash = legacy_hash("abcd", "pw");
        assert!(a.check_password("pw"));
        assert!(!a.check_password("nope"));
        assert!(a.needs_rehash());
        a.set_password("pw");
        assert!(!a.needs_rehash());
        assert!(a.hash.starts_with("$argon2id$"));
        assert!(a.check_password("pw"));
    }

    #[test]
    fn passwords() {
        let a = Account::new("Lefty", "hunter2", "x@y", Level::User);
        assert!(a.check_password("hunter2"));
        assert!(!a.check_password("hunter3"));
    }

    #[test]
    fn persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.json");
        let mut db = Db::load(Some(path.clone())).unwrap();
        db.insert(Account::new("Lefty", "pw", "e", Level::Admin));
        write_snapshot(db.snapshot());
        let db = Db::load(Some(path)).unwrap();
        assert_eq!(db.get("LEFTY").unwrap().level, Level::Admin);
        assert!(db.get("lefty").unwrap().check_password("pw"));
    }

    #[test]
    fn ban_matching() {
        let b = Ban { target: "10.0.".into(), by: "op".into(), reason: String::new(), when: 0 };
        assert!(b.matches("x", "10.0.3.4"));
        assert!(!b.matches("x", "11.0.3.4"));
    }
}
