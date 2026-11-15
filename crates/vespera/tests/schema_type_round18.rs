//! Round-18 `schema_type!` fixes:
//!
//! - M1: a bare source name inside an inline `mod` named a same-named
//!   `#[derive(Schema)]` of a sibling inline `mod` of the same file;
//! - L1: a `rename`d peer of a garde `matches(..)` / `self.peer` rule kept
//!   its old name (E0609);
//! - L6: a source `#[serde(remote = "..")]` turned the DTO's serde derives
//!   into inherent functions for the remote type.
#![deny(warnings)]

use garde::Validate;

mod a {
    #[derive(vespera::Schema, serde::Serialize, serde::Deserialize, Clone)]
    pub struct User {
        pub x: i32,
    }
}

mod b {
    #[derive(Clone)]
    pub struct User {
        pub y: String,
    }

    vespera::schema_type!(UserDto from User, ignore);
}

mod m {
    use serde::{Deserialize, Serialize};

    #[derive(Deserialize, Clone, garde::Validate)]
    pub struct Signup {
        #[garde(length(min = 2))]
        pub password: String,
        #[garde(matches(password), custom(differs_from(&self.password)))]
        pub confirm: String,
    }

    pub fn differs_from(other: &str) -> impl FnOnce(&str, &()) -> garde::Result + '_ {
        move |value, ()| {
            if value == other {
                Ok(())
            } else {
                Err(garde::Error::new("differs"))
            }
        }
    }

    pub struct Span {
        pub secs: i64,
    }

    #[derive(Serialize, Deserialize, Clone)]
    #[serde(remote = "Span")]
    pub struct SpanDef {
        pub secs: i64,
    }
}

vespera::schema_type!(SignupDto from crate::m::Signup, ignore, rename = [("password", "pw")]);

vespera::schema_type!(SpanDto from crate::m::SpanDef, ignore);

#[test]
fn bare_name_in_inline_module_is_that_modules_struct() {
    let dto: b::UserDto = b::User { y: "y".into() }.into();
    assert_eq!(dto.y, "y");
    assert_eq!(a::User { x: 1 }.x, 1);
}

#[test]
fn renamed_garde_peer_is_followed() {
    let ok: SignupDto = serde_json::from_str(r#"{"password":"ab","confirm":"ab"}"#).unwrap();
    assert!(ok.validate().is_ok());
    let mismatch: SignupDto = serde_json::from_str(r#"{"password":"ab","confirm":"cd"}"#).unwrap();
    assert!(mismatch.validate().is_err());
}

#[test]
fn remote_container_attr_is_not_copied() {
    let dto: SpanDto = serde_json::from_str(r#"{"secs":3}"#).unwrap();
    assert_eq!(serde_json::to_string(&dto).unwrap(), r#"{"secs":3}"#);
    let from: SpanDto = m::SpanDef { secs: 4 }.into();
    assert_eq!(from.secs, 4);
}
