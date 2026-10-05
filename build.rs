//! Embeds the commit SHA and build target so `speq --version` can report them.
use std::process::Command;

fn main() {
    let commit = std::env::var("SPEQ_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=SPEQ_COMMIT={commit}");
    println!("cargo:rustc-env=SPEQ_TARGET={target}");
    println!("cargo:rerun-if-env-changed=SPEQ_COMMIT");
    println!("cargo:rerun-if-changed=.git/HEAD");
}
