// Example program to extract Rust configuration defaults as JSON.
// Run with: cargo run -p conduit-config --example extract_defaults

use conduit_config::AppConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let defaults = AppConfig::default();
    let json = serde_json::to_string_pretty(&defaults)?;

    // Write to stdout
    print!("{json}");

    Ok(())
}
