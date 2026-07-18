use std::process::Command;

fn main() {
    // Capture the rustc version that compiled THIS copy of tradectl-sdk.
    //
    // Downstream crates (the tradectl CLI and every strategy dylib) each
    // recompile tradectl-sdk with their own active toolchain, so this env
    // reflects the compiler on each side of the plugin ABI boundary. It is
    // embedded via `env!` into `abi::SDK_RUSTC_VERSION` and surfaced only in the
    // load-time error message when a plugin's layout fingerprint does not match
    // the host CLI (2026-07-14 bnum/bncm incident: CLI on rustc 1.97.0 vs a
    // strategy hand-built on 1.95.0 → mismatched #[repr(Rust)] layout).
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=TRADECTL_RUSTC_VERSION={version}");
    println!("cargo:rerun-if-changed=build.rs");
}
