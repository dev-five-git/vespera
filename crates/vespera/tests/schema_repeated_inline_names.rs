//! Two inline `mod`s of one file each define a `#[derive(Schema)]` struct of
//! the same name whose `#[serde(default = "..")]` names a function of its own
//! module: the derive picks its module by the struct's line, which needs the
//! file re-parsed with real span positions (inside the compiler every
//! re-parsed token points at the call site, so no candidate matched and the
//! default was "not statically determinable" — a compile error).
#![deny(warnings)]

mod first {
    use serde::{Deserialize, Serialize};
    use vespera::Schema;

    fn limit() -> u32 {
        10
    }

    #[derive(Serialize, Deserialize, Clone, Schema)]
    pub struct Page {
        #[serde(default = "limit")]
        pub limit: u32,
    }
}

mod second {
    use serde::{Deserialize, Serialize};
    use vespera::Schema;

    fn limit() -> u32 {
        20
    }

    #[derive(Serialize, Deserialize, Clone, Schema)]
    pub struct Page {
        #[serde(default = "limit")]
        pub limit: u32,
    }
}

#[test]
fn each_page_resolves_its_own_default_fn() {
    let first: first::Page = serde_json::from_str("{}").unwrap();
    let second: second::Page = serde_json::from_str("{}").unwrap();
    assert_eq!((first.limit, second.limit), (10, 20));
}
