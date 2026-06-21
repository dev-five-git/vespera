use vespera_macro::route;

#[route(get, post)]
pub async fn duplicate_method() {}

#[route(get, path = "/one", path = "/two")]
pub async fn duplicate_path() {}

#[route(post, status = 201, status = 202)]
pub async fn duplicate_status() {}

#[route(get, responses = [(404, Missing)], responses = [(500, Broken)])]
pub async fn duplicate_responses() {}

fn main() {}
