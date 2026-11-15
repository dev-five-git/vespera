//! Round-17 `schema_type!` fixes:
//!
//! - M1: `partial` kept a wrapped field's `#[garde(..)]` rules on the
//!   `Option`: `inner(..)` then checked the `Option`'s value instead of its
//!   elements, `custom(..)` got `&Option<T>` (E0308) and `matches(other)`
//!   against an unwrapped field compared `Option<T>` with `T` (E0277);
//! - M3: a `Self` in a source field type was emitted as
//!   `crate::m::Self` (E0433).
#![deny(warnings)]

use garde::Validate;

mod m {
    use serde::{Deserialize, Serialize};

    #[derive(Deserialize, Clone, garde::Validate)]
    pub struct Profile {
        // Every tag non-empty.
        #[garde(inner(length(min = 1)))]
        pub tags: Vec<String>,
        #[garde(custom(check_name), length(max = 5))]
        pub name: String,
        #[garde(skip)]
        pub password: String,
        #[garde(matches(password))]
        pub confirm: String,
        #[garde(length(min = 1))]
        #[cfg_attr(all(), garde(custom(check_name)))]
        pub nick: String,
        #[cfg_attr(all(), garde(custom(check_name)))]
        pub alias: String,
        #[garde(dive)]
        pub inner: Inner,
    }

    #[derive(Serialize, Deserialize, Clone, garde::Validate)]
    pub struct Inner {
        #[garde(length(min = 1))]
        pub value: String,
    }

    #[derive(vespera::Schema, Serialize, Deserialize, Clone)]
    pub struct Category {
        pub id: i32,
        pub children: Vec<Self>,
        pub parent: Option<Box<Self>>,
    }

    // garde passes the context by reference.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn check_name(value: &str, (): &()) -> garde::Result {
        if value.starts_with('x') {
            return Err(garde::Error::new("no x"));
        }
        Ok(())
    }
}

// `password` stays required: `confirm` (wrapped) matches an unwrapped field.
vespera::schema_type!(ProfilePatch from crate::m::Profile, ignore, partial = ["tags", "name", "confirm", "nick", "alias", "inner"]);
// Both sides of `matches` wrapped.
vespera::schema_type!(ProfileAll from crate::m::Profile, ignore, partial);

vespera::schema_type!(CategoryDto from crate::m::Category);

fn patch(json: &str) -> ProfilePatch {
    serde_json::from_str(json).unwrap()
}

#[test]
fn partial_garde_rules_apply_to_the_some_value() {
    assert!(patch(r#"{"password":"p"}"#).validate().is_ok());
    // `inner(..)` still checks each tag, not the Vec.
    assert!(patch(r#"{"password":"p","tags":[]}"#).validate().is_ok());
    assert!(patch(r#"{"password":"p","tags":["a"]}"#).validate().is_ok());
    assert!(patch(r#"{"password":"p","tags":[""]}"#).validate().is_err());
    // `custom(..)` gets the `Some` value.
    assert!(patch(r#"{"password":"p","name":"abc"}"#).validate().is_ok());
    assert!(
        patch(r#"{"password":"p","name":"xab"}"#)
            .validate()
            .is_err()
    );
    assert!(
        patch(r#"{"password":"p","name":"abcdefg"}"#)
            .validate()
            .is_err()
    );
    // `cfg_attr` garde items move into `inner(..)` too.
    assert!(patch(r#"{"password":"p","nick":"xa"}"#).validate().is_err());
    assert!(patch(r#"{"password":"p","nick":""}"#).validate().is_err());
    assert!(
        patch(r#"{"password":"p","alias":"xa"}"#)
            .validate()
            .is_err()
    );
    assert!(patch(r#"{"password":"p","alias":"a"}"#).validate().is_ok());
    // `matches` an unwrapped field compares the `Some` value.
    assert!(
        patch(r#"{"password":"p","confirm":"p"}"#)
            .validate()
            .is_ok()
    );
    assert!(
        patch(r#"{"password":"p","confirm":"q"}"#)
            .validate()
            .is_err()
    );
    // `dive` keeps validating the nested value.
    assert!(
        patch(r#"{"password":"p","inner":{"value":""}}"#)
            .validate()
            .is_err()
    );
    assert!(
        patch(r#"{"password":"p","inner":{"value":"v"}}"#)
            .validate()
            .is_ok()
    );
}

#[test]
fn partial_garde_matches_between_two_wrapped_fields() {
    let all = |json: &str| serde_json::from_str::<ProfileAll>(json).unwrap();
    assert!(all("{}").validate().is_ok());
    assert!(all(r#"{"password":"p","confirm":"p"}"#).validate().is_ok());
    assert!(all(r#"{"password":"p","confirm":"q"}"#).validate().is_err());
    assert!(all(r#"{"tags":[""]}"#).validate().is_err());
}

#[test]
fn self_in_a_field_type_names_the_source() {
    let leaf = m::Category {
        id: 2,
        children: Vec::new(),
        parent: None,
    };
    let dto = CategoryDto::from(m::Category {
        id: 1,
        children: vec![leaf.clone()],
        parent: Some(Box::new(leaf)),
    });
    let children: &Vec<m::Category> = &dto.children;
    assert_eq!((dto.id, children[0].id), (1, 2));
    assert_eq!(dto.parent.map(|parent| parent.id), Some(2));
}
