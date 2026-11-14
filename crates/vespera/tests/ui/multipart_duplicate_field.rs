//! Compile-fail: two fields of a `#[derive(Multipart)]` struct that resolve
//! to the same multipart field name. The second `match` arm would be
//! unreachable, leaving that field permanently missing — a compile error
//! names both fields instead.

#[derive(vespera::Multipart)]
pub struct Upload {
    pub file: String,
    #[form_data(field_name = "file")]
    pub upload: String,
}

fn main() {}
