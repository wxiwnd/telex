use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Normalized address: `local@domain`, lowercase.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AddressError {
    #[error("address is empty")]
    Empty,
    #[error("multiple @ in address: {0}")]
    MultipleAt(String),
    #[error("missing domain in address: {0}")]
    NoDomain(String),
    #[error("invalid local-part: {0}")]
    BadLocal(String),
    #[error("invalid domain: {0}")]
    BadDomain(String),
}

impl Address {
    /// Parse and normalize. Appends `default_domain` when `@` is missing.
    /// Letters are lowercased.
    pub fn parse(input: &str, default_domain: &str) -> Result<Self, AddressError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(AddressError::Empty);
        }
        let lower = trimmed.to_ascii_lowercase();
        let full = if lower.contains('@') {
            lower
        } else {
            format!("{lower}@{}", default_domain.to_ascii_lowercase())
        };

        let mut parts = full.split('@');
        let local = parts.next().unwrap_or_default();
        let domain = match parts.next() {
            Some(d) => d,
            None => return Err(AddressError::NoDomain(full)),
        };
        if parts.next().is_some() {
            return Err(AddressError::MultipleAt(full));
        }

        validate_local(local).map_err(|_| AddressError::BadLocal(local.to_string()))?;
        validate_domain(domain).map_err(|_| AddressError::BadDomain(domain.to_string()))?;

        Ok(Self(full))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn local_part(&self) -> &str {
        self.0.split_once('@').map(|(l, _)| l).unwrap_or(&self.0)
    }

    pub fn domain(&self) -> &str {
        self.0.split_once('@').map(|(_, d)| d).unwrap_or_default()
    }
}

fn is_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn validate_local(local: &str) -> Result<(), ()> {
    if local.is_empty() || local.len() > 64 {
        return Err(());
    }
    let mut chars = local.chars();
    if !chars.next().is_some_and(is_alnum) {
        return Err(());
    }
    if local
        .chars()
        .any(|c| !(is_alnum(c) || c == '.' || c == '_' || c == '-'))
    {
        return Err(());
    }
    Ok(())
}

fn validate_domain(domain: &str) -> Result<(), ()> {
    if domain.is_empty() || domain.len() > 64 {
        return Err(());
    }
    for label in domain.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(());
        }
        let mut chars = label.chars();
        if !chars.next().is_some_and(is_alnum) {
            return Err(());
        }
        if label.chars().last().is_some_and(|c| !is_alnum(c)) {
            return Err(());
        }
        if label.chars().any(|c| !(is_alnum(c) || c == '-')) {
            return Err(());
        }
    }
    Ok(())
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Address {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Address::parse(&raw, "local").map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_and_domain() {
        assert_eq!(
            Address::parse("Alice", "local").unwrap().as_str(),
            "alice@local"
        );
        assert_eq!(
            Address::parse(" Bob@Example.COM ", "local")
                .unwrap()
                .as_str(),
            "bob@example.com"
        );
        assert_eq!(
            Address::parse("codex.worker@local", "local")
                .unwrap()
                .local_part(),
            "codex.worker"
        );
        assert_eq!(
            Address::parse("Alice", "EXAMPLE.COM").unwrap().as_str(),
            "alice@example.com"
        );
    }

    #[test]
    fn rejects_bad_addresses() {
        assert!(Address::parse("", "local").is_err());
        assert!(Address::parse("a@@b", "local").is_err());
        assert!(Address::parse(".alice@local", "local").is_err());
        assert!(Address::parse("alice@-bad", "local").is_err());
        assert!(Address::parse("alice@bad..domain", "local").is_err());
        assert!(Address::parse("ali ce@local", "local").is_err());
    }
}
