//! `fire_decode::VERSION` is `product.json`'s version: the one place it is set.

#[test]
fn the_version_is_product_jsons() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../product.json");
    let raw = std::fs::read_to_string(path).expect("product.json at the repository's root");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("product.json is JSON");
    assert_eq!(Some(fire_decode::VERSION), json["version"].as_str());
}
