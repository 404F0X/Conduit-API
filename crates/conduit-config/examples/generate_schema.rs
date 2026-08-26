fn main() {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.schema.json".to_string());

    if let Err(error) = conduit_config::schema::write_schema(&output) {
        eprintln!("failed to write configuration schema to {output}: {error}");
        std::process::exit(1);
    }
}
