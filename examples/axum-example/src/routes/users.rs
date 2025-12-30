use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use vespera::{
    Schema,
    axum::{Json, extract::Path},
};

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct User {
    pub id: u32,
    pub name: String,
    pub email: String,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct CreateUserRequest {
    pub name: String,
    pub email: String,
}

/// Get all users
#[vespera::route(get)]
pub async fn get_users() -> Json<Vec<User>> {
    Json(vec![
        User {
            id: 1,
            name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
        },
        User {
            id: 2,
            name: "Bob".to_string(),
            email: "bob@example.com".to_string(),
        },
    ])
}

/// Get user by ID
#[vespera::route(get, path = "/{id}")]
pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
    Json(User {
        id,
        name: format!("User {}", id),
        email: format!("user{}@example.com", id),
    })
}

/// Create a new user
#[vespera::route(post)]
pub async fn create_user(Json(user): Json<CreateUserRequest>) -> Json<User> {
    Json(User {
        id: 100,
        name: user.name,
        email: user.email,
    })
}

#[derive(Serialize, Deserialize, Schema)]
pub struct SkipResponse {
    pub name: String,
    #[serde(skip)]
    #[allow(dead_code)]
    pub email: String,

    #[serde(skip, skip_serializing_if = "Option::is_none")]
    #[allow(dead_code)]
    pub email2: Option<String>,

    #[serde(rename = "email3", skip)]
    #[allow(dead_code)]
    pub email3: Option<String>,

    #[serde(rename = "email4", skip_serializing_if = "Option::is_none")]
    pub email4: Option<String>,

    #[serde(rename = "email5", default)]
    pub email5: String,

    #[serde(rename = "email6", default = "default_value")]
    pub email6: String,

    #[serde(rename = "email7", skip)]
    #[allow(dead_code)]
    pub email7: String,

    #[serde(rename = "num", default)]
    pub num: i32,

    pub in_skip: InSkipResponse,
    pub in_skip2: Option<InSkipResponse>,
    pub in_skip3: Vec<InSkipResponse>,
    pub in_skip4: Option<Vec<InSkipResponse>>,
    pub in_skip5: Option<HashMap<String, InSkipResponse>>,
    pub in_skip6: Option<BTreeMap<String, InSkipResponse>>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct InSkipResponse {
    pub name: String,
}

fn default_value() -> String {
    "default42".to_string()
}

#[vespera::route(get, path = "/skip-response")]
pub async fn skip_response() -> Json<SkipResponse> {
    Json(SkipResponse {
        name: "John Doe".to_string(),
        email: "john.doe@example.com".to_string(),
        email2: Some("john.doe2@example.com".to_string()),
        email3: Some("john.doe3@example.com".to_string()),
        email4: Some("john.doe4@example.com".to_string()),
        email5: "john.doe5@example.com".to_string(),
        email6: "john.doe6@example.com".to_string(),
        email7: "john.doe7@example.com".to_string(),
        num: 0,
        in_skip: InSkipResponse {
            name: "John Doe".to_string(),
        },
        in_skip2: Some(InSkipResponse {
            name: "John Doe".to_string(),
        }),
        in_skip3: vec![InSkipResponse {
            name: "John Doe".to_string(),
        }],
        in_skip4: Some(vec![InSkipResponse {
            name: "John Doe".to_string(),
        }]),
        in_skip5: Some(HashMap::from([(
            "John Doe".to_string(),
            InSkipResponse {
                name: "John Doe".to_string(),
            },
        )])),
        in_skip6: Some(BTreeMap::from([(
            "John Doe".to_string(),
            InSkipResponse {
                name: "John Doe".to_string(),
            },
        )])),
    })
}
