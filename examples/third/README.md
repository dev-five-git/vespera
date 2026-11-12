# third

A minimal vespera app used to exercise **app merging**. It defines a route
folder and a `#[derive(Schema)]` type, then exports itself with
`vespera::export_app!(ThirdApp)`, which generates `ThirdApp::router()` and
`ThirdApp::OPENAPI_SPEC`. [`axum-example`](../axum-example) pulls it in via
`vespera!(..., merge = [ThirdApp])`, so its routes and schemas appear in the
merged router and `openapi.json`. It is not meant to be run on its own.
