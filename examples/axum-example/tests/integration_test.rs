use axum_example::create_app;
use axum_test::TestServer;
use serde_json::json;

#[tokio::test]
async fn test_health_endpoint() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/health").await;

    response.assert_status_ok();
    response.assert_text("ok");
}

#[tokio::test]
async fn test_mod_file_endpoint() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/hello").await;

    response.assert_status_ok();
    response.assert_text("mod file endpoint");

    let response = server.get("/").await;

    response.assert_status_ok();
    response.assert_text("root endpoint");
}

#[tokio::test]
async fn test_get_users() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/users").await;

    response.assert_status_ok();
    let users: serde_json::Value = response.json();

    assert!(users.is_array());
    assert_eq!(users.as_array().unwrap().len(), 2);

    let first_user = &users[0];
    assert_eq!(first_user["id"], 1);
    assert_eq!(first_user["name"], "Alice");
    assert_eq!(first_user["email"], "alice@example.com");
}

#[tokio::test]
async fn test_get_user_by_id() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/users/42").await;

    response.assert_status_ok();
    let user: serde_json::Value = response.json();

    assert_eq!(user["id"], 42);
    assert_eq!(user["name"], "User 42");
    assert_eq!(user["email"], "user42@example.com");
}

#[tokio::test]
async fn test_create_user() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let new_user = json!({
        "name": "Charlie",
        "email": "charlie@example.com"
    });

    let response = server.post("/users").json(&new_user).await;

    response.assert_status_ok();
    let created_user: serde_json::Value = response.json();

    assert_eq!(created_user["id"], 100);
    assert_eq!(created_user["name"], "Charlie");
    assert_eq!(created_user["email"], "charlie@example.com");
}

#[tokio::test]
async fn test_get_nonexistent_user() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/users/999").await;

    response.assert_status_ok();
    let user: serde_json::Value = response.json();
    assert_eq!(user["id"], 999);
}

#[tokio::test]
async fn test_prefix_variable() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/path/prefix/123").await;

    response.assert_status_ok();
    response.assert_text("prefix variable: 123");
}

#[tokio::test]
async fn test_invalid_path() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let response = server.get("/nonexistent").await;

    response.assert_status_not_found();
}

#[tokio::test]
async fn test_mod_file_with_complex_struct_body() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let complex_body = json!({
        "name": "Test User",
        "age": 30,
        "nested_struct": {
            "name": "Nested Name",
            "age": 25
        },
        "array": ["item1", "item2", "item3"],
        "map": {
            "key1": "value1",
            "key2": "value2"
        },
        "nested_array": [
            {
                "name": "Array Item 1",
                "age": 20
            },
            {
                "name": "Array Item 2",
                "age": 21
            }
        ],
        "nested_map": {
            "map_key1": {
                "name": "Map Value 1",
                "age": 22
            },
            "map_key2": {
                "name": "Map Value 2",
                "age": 23
            }
        },
        "nested_struct_array": [
            {
                "name": "Struct Array 1",
                "age": 24
            }
        ],
        "nested_struct_map": {
            "struct_map_key": {
                "name": "Struct Map Value",
                "age": 26
            }
        },
        "nested_struct_array_map": [
            {
                "array_map_key1": {
                    "name": "Array Map Value 1",
                    "age": 27
                },
                "array_map_key2": {
                    "name": "Array Map Value 2",
                    "age": 28
                }
            }
        ],
        "nested_struct_map_array": {
            "map_array_key": [
                {
                    "name": "Map Array Value 1",
                    "age": 29
                },
                {
                    "name": "Map Array Value 2",
                    "age": null
                }
            ]
        }
    });

    let response = server
        .post("/complex-struct-body")
        .json(&complex_body)
        .await;

    response.assert_status_ok();
    let response_text = response.text();

    assert!(response_text.contains("name: Test User"));
    assert!(response_text.contains("age: 30"));
    assert!(response_text.contains("item1"));
    assert!(response_text.contains("value1"));
}

#[tokio::test]
async fn test_mod_file_with_complex_struct_body_with_rename() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();

    let complex_body = json!({
        "name": "Test User Renamed",
        "age": 35,
        "nestedStruct": {
            "name": "Nested Name Renamed",
            "age": 30
        },
        "array": ["renamed1", "renamed2", "renamed3"],
        "map": {
            "key1": "renamed_value1",
            "key2": "renamed_value2"
        },
        "nestedArray": [
            {
                "name": "Renamed Array Item 1",
                "age": 25
            },
            {
                "name": "Renamed Array Item 2",
                "age": 26
            }
        ],
        "nestedMap": {
            "map_key1": {
                "name": "Renamed Map Value 1",
                "age": 27
            },
            "map_key2": {
                "name": "Renamed Map Value 2",
                "age": 28
            }
        },
        "nestedStructArray": [
            {
                "name": "Renamed Struct Array 1",
                "age": 29
            }
        ],
        "nestedStructMap": {
            "struct_map_key": {
                "name": "Renamed Struct Map Value",
                "age": 31
            }
        },
        "nestedStructArrayMap": [
            {
                "array_map_key1": {
                    "name": "Renamed Array Map Value 1",
                    "age": 32
                },
                "array_map_key2": {
                    "name": "Renamed Array Map Value 2",
                    "age": 33
                }
            }
        ],
        "nestedStructMapArray": {
            "map_array_key": [
                {
                    "name": "Renamed Map Array Value 1",
                    "age": 34
                },
                {
                    "name": "Renamed Map Array Value 2",
                    "age": null
                }
            ]
        }
    });

    let response = server
        .post("/complex-struct-body-with-rename")
        .json(&complex_body)
        .await;

    response.assert_status_ok();
    let response_text = response.text();

    assert!(response_text.contains("name: Test User Renamed"));
    assert!(response_text.contains("age: 35"));
    assert!(response_text.contains("renamed1"));
    assert!(response_text.contains("renamed_value1"));
}

#[tokio::test]
async fn test_openapi() {
    insta::assert_snapshot!("openapi", std::fs::read_to_string("openapi.json").unwrap());
}
