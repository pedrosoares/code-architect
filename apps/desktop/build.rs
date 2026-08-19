//! Bakes the four OAuth client id/secret values into the binary at compile
//! time (`env!()` in `src/oauth.rs`), sourced from environment variables —
//! a workspace-root `.env` file for local development, or real exported
//! env vars (GitHub Actions secrets in CI). This keeps the "click Login,
//! no setup" UX for a distributed binary (see `oauth.rs`'s doc comment for
//! why these can't just be read at runtime) while keeping the actual
//! values out of source control.

fn main() {
    let root_env = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env");

    // Local dev: load the workspace-root .env into this process's
    // environment if present. Harmless no-op otherwise — CI and a
    // contributor's machine with no .env both just get empty strings
    // below, same as today's unconfigured state.
    let _ = dotenvy::from_path(&root_env).or_else(|_| dotenvy::dotenv().map(|_| ()));

    for var in [
        "GITHUB_CLIENT_ID",
        "GITHUB_CLIENT_SECRET",
        "SLACK_CLIENT_ID",
        "LINEAR_CLIENT_ID",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
        println!(
            "cargo:rustc-env={var}={}",
            std::env::var(var).unwrap_or_default()
        );
    }
    println!("cargo:rerun-if-changed={}", root_env.display());
}
