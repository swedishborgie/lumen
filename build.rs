fn main() {
    // Allow the CI/Docker pipeline to inject the release version via LUMEN_VERSION
    // (e.g. "0.2.0" stripped from a "v0.2.0" git tag).  Falls back to the version
    // declared in Cargo.toml when building locally without the env var set.
    let version = std::env::var("LUMEN_VERSION")
        .or_else(|_| std::env::var("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=LUMEN_VERSION={version}");
    println!("cargo:rerun-if-env-changed=LUMEN_VERSION");
}
