//! Address grammar for APIs, plugins, methods, points, and events.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub api: String,
    pub segment: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AddressError {
    #[error("address `{0}` is not <api>.<name>")]
    Shape(String),
    #[error("name `{0}` is not [a-z][a-z0-9_]*")]
    Name(String),
}

#[must_use]
pub fn valid_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {},
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

pub fn parse_address(value: &str) -> Result<Address, AddressError> {
    let Some((api, segment)) = value.split_once('.') else {
        return Err(AddressError::Shape(value.to_owned()));
    };
    if segment.contains('.') || !valid_name(api) || !valid_name(segment) {
        return Err(AddressError::Shape(value.to_owned()));
    }
    Ok(Address {
        api: api.to_owned(),
        segment: segment.to_owned(),
    })
}

#[must_use]
pub fn is_rpc(value: &str) -> bool {
    value.starts_with("rpc.")
}

#[must_use]
pub fn is_bus(value: &str) -> bool {
    value.starts_with("bus.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dotted_names() {
        let address = parse_address("config.get").expect("address");
        assert_eq!(address.api, "config");
        assert_eq!(address.segment, "get");
    }

    #[test]
    fn rejects_bad_shapes() {
        assert!(parse_address("config").is_err());
        assert!(parse_address("Config.get").is_err());
        assert!(parse_address("config.Get").is_err());
        assert!(parse_address("a.b.c").is_err());
    }
}
