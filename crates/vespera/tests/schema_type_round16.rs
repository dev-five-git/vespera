//! Round-16 `schema_type!` source shapes that used to fail to compile:
//!
//! - M1: `#[garde(custom(Self::check_name))]` was copied verbatim onto the
//!   DTO, where `Self` is the DTO (E0599);
//! - M3: in a file loaded through `#[path]`, the field types were rooted at
//!   the module the file's place on disk names (E0433);
//! - L4: `#[garde(length(max = MAX_NAME as usize))]` (and index / array /
//!   tuple / range / block expressions) kept a source-module constant
//!   un-rooted (E0425 at a call site in another module);
//! - L5: `#[serde(default = "Self::default_size")]` on a generic source was
//!   spelled `Page::default_size` (E0282: `T` cannot be inferred);
//! - L8: `#[serde(default = "Limit::<u8>::get")]` lost its arguments when
//!   re-rooted (E0282 / wrong instance).
#![deny(warnings)]

use garde::Validate;

mod m {
    use serde::{Deserialize, Serialize};
    use vespera::Schema;

    pub const MAX_NAME: u32 = 5;

    pub struct Limit<T>(T);

    impl Limit<u8> {
        pub fn get() -> u32 {
            8
        }
    }

    impl Limit<u16> {
        pub fn get() -> u32 {
            16
        }
    }

    #[derive(Deserialize, Clone, garde::Validate)]
    pub struct CreateUser {
        #[garde(custom(Self::check_name), length(max = MAX_NAME as usize))]
        pub name: String,
        #[garde(range(max = [MAX_NAME][0]))]
        pub age: u32,
        #[serde(default = "Limit::<u8>::get")]
        #[garde(skip)]
        pub limit: u32,
    }

    impl CreateUser {
        // garde passes the context by reference.
        #[allow(clippy::trivially_copy_pass_by_ref)]
        pub(crate) fn check_name(value: &str, (): &()) -> garde::Result {
            if value.starts_with('x') {
                return Err(garde::Error::new("no x"));
            }
            Ok(())
        }
    }

    #[derive(Schema, Serialize, Clone)]
    pub struct Page<T> {
        pub items: Vec<T>,
        #[serde(default = "Self::default_size")]
        pub size: u32,
    }

    impl<T> Page<T> {
        pub(crate) fn default_size() -> u32 {
            25
        }
    }
}

#[derive(vespera::Schema, serde::Serialize, serde::Deserialize, Clone)]
pub struct User {
    pub id: i32,
}

vespera::schema_type!(UserDto from crate::m::CreateUser, ignore);
vespera::schema_type!(UserPage from crate::m::Page<User>);

#[test]
fn garde_self_and_casts_resolve_on_the_dto() {
    let dto: UserDto = serde_json::from_str(r#"{"name":"abc","age":1}"#).unwrap();
    assert_eq!((dto.limit, m::Limit::<u16>::get()), (8, 16));
    assert!(dto.validate().is_ok());
    let custom: UserDto = serde_json::from_str(r#"{"name":"xab","age":1}"#).unwrap();
    assert!(custom.validate().is_err());
    let long: UserDto = serde_json::from_str(r#"{"name":"abcdefg","age":1}"#).unwrap();
    assert!(long.validate().is_err());
    let old: UserDto = serde_json::from_str(r#"{"name":"abc","age":9}"#).unwrap();
    assert!(old.validate().is_err());
}

#[path = "schema_type_fixtures/path_models.rs"]
mod models;

#[test]
fn path_loaded_module_roots_its_field_types() {
    let dto = models::MemberDto::from(models::Member {
        id: 1,
        kind: models::Kind::Admin,
    });
    assert_eq!((dto.id, dto.kind), (1, models::Kind::Admin));
    let member: models::MemberDto = serde_json::from_str(r#"{"id":2,"kind":"Member"}"#).unwrap();
    assert_eq!(member.kind, models::Kind::Member);
}

#[test]
fn generic_self_default_is_instantiated() {
    let page: UserPage = serde_json::from_str(r#"{"items":[{"id":1}]}"#).unwrap();
    assert_eq!((page.items[0].id, page.size), (1, 25));
}
