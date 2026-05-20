//! Print the JSON Schema for [`ezu_style::Style`] to stdout.
//!
//! Used to regenerate `schemas/ezu-style.json` whenever the spec changes:
//!
//! ```sh
//! cargo run --bin dump-schema -p ezu-style > schemas/ezu-style.json
//! ```

fn main() {
    let schema = schemars::schema_for!(ezu_style::Style);
    let json = serde_json::to_string_pretty(&schema).expect("serialize schema");
    println!("{json}");
}
