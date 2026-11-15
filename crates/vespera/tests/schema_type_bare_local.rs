//! A bare `schema_type!` source names the struct defined in the call-site
//! file: a same-named `#[derive(Schema)]` of another module (registered
//! first) used to win, giving the DTO that type's fields while the generated
//! `From` impl named the local struct.

#[path = "schema_type_fixtures/bare_remote.rs"]
mod remote;

use serde::{Deserialize, Serialize};
use vespera::schema_type;

#[derive(Serialize, Deserialize, Clone)]
pub struct User {
    pub id: u32,
    pub name: String,
}

schema_type!(UserDto from User);

#[test]
fn bare_source_uses_the_call_site_definition() {
    let dto = UserDto::from(User {
        id: 7,
        name: "local".to_string(),
    });
    assert_eq!((dto.id, dto.name.as_str()), (7, "local"));
    // The other module's `User` is untouched.
    let remote = remote::User {
        id: 1,
        email: "a@b".to_string(),
    };
    assert_eq!(remote.email, "a@b");
}
