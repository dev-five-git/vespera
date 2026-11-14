use serde::Serialize;
use vespera::{Schema, axum::Json};

use crate::TestStruct;

#[derive(Serialize, vespera::Schema)]
pub struct GenericStruct<T: Serialize> {
    pub value: T,
    pub name: String,
}

#[derive(Serialize, vespera::Schema)]
pub struct GenericStruct2<T, T2> {
    pub value: T,
    pub name: String,
    pub value2: T2,
}

#[vespera::route(get, path = "/generic/{value}")]
pub async fn generic_endpoint(
    vespera::axum::extract::Path(value): vespera::axum::extract::Path<String>,
) -> Json<GenericStruct<String>> {
    Json(GenericStruct {
        value,
        name: "John Doe".to_string(),
    })
}

#[vespera::route(get, path = "/generic2")]
pub async fn generic_endpoint2() -> Json<GenericStruct<TestStruct>> {
    Json(GenericStruct {
        value: TestStruct {
            name: "test".to_string(),
            age: 20,
        },
        name: "John Doe".to_string(),
    })
}

#[vespera::route(get, path = "/generic3")]
pub async fn generic_endpoint3() -> Json<GenericStruct2<TestStruct, String>> {
    Json(GenericStruct2 {
        value: TestStruct {
            name: "test".to_string(),
            age: 20,
        },
        value2: "test2".to_string(),
        name: "John Doe".to_string(),
    })
}

#[vespera::route(get, path = "/generic4")]
pub async fn generic_endpoint4() -> Json<GenericStruct2<bool, bool>> {
    Json(GenericStruct2 {
        value: true,
        value2: false,
        name: "John Doe".to_string(),
    })
}
#[derive(Debug, Serialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct ContactResponse {
    pub id: i64,
    pub user_id: i64,
    pub category: Option<String>,
    pub title: String,
    pub content: String,
    pub admin_reply: Option<String>,
    pub replied_at: Option<String>,
    pub created_at: String,
    pub updated_at: Option<String>,
}
#[derive(Debug, Serialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct PaginatedResponse<T: Serialize> {
    pub items: Vec<T>,
    pub page: i32,
    pub size: i32,
    pub total_page: i32,
}
#[vespera::route(get, path = "/generic5")]
pub async fn generic_endpoint5()
-> Result<Json<PaginatedResponse<ContactResponse>>, (vespera::axum::http::StatusCode, String)> {
    Ok(Json(PaginatedResponse {
        items: vec![ContactResponse {
            id: 1,
            user_id: 1,
            category: Some("test".to_string()),
            title: "test".to_string(),
            content: "test".to_string(),
            admin_reply: None,
            replied_at: None,
            created_at: "2021-01-01".to_string(),
            updated_at: None,
        }],
        page: 1,
        size: 10,
        total_page: 1,
    }))
}

/// A row type the generic source module (`models::page`) cannot name.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize, Schema)]
pub struct PageRow {
    pub id: i32,
}

// A generic source in another module: the call-site argument `PageRow` is
// inserted as written (it used to resolve as `crate::models::page::PageRow`),
// the source's own `Cursor` resolves in `models::page`, and the default
// `M = Vec<T>` gets `T`'s value.
vespera::schema_type!(RowPage from crate::models::page::Page<PageRow>);

#[cfg(test)]
mod generic_source_tests {
    use super::*;

    #[test]
    fn generic_source_from_another_module() {
        let page = crate::models::page::Page {
            items: vec![PageRow { id: 1 }],
            cursor: crate::models::page::Cursor { next: None },
            sample: vec![PageRow { id: 2 }],
        };
        let dto = RowPage::from(page);
        let sample: &Vec<PageRow> = &dto.sample;
        assert_eq!((dto.items[0].id, sample[0].id), (1, 2));
        assert_eq!(dto.cursor.next, None);
    }
}
