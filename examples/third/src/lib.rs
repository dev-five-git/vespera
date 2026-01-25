mod routes;

use serde::{Deserialize, Serialize};
use vespera::Schema;

#[derive(Serialize, Deserialize, Schema)]
pub struct TestStruct {
    pub name: String,
    pub age: u32,
}

// Export the app for merging by other vespera apps
// dir defaults to "routes" when omitted
vespera::export_app!(ThirdApp);
