use std::{
    env,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
fn main() {
    println!("cargo:rerun-if-env-changed=GRIDSTATOR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=GRIDSTATOR_SOURCE_REVISION");
    println!("cargo:rerun-if-changed=src");
    let revision = env::var("GRIDSTATOR_SOURCE_REVISION").unwrap_or_else(|_| {
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_else(|| "unknown".into())
    });
    let build = env::var("GRIDSTATOR_BUILD_ID").unwrap_or_else(|_| {
        format!(
            "dev-{}-{}",
            &revision[..revision.len().min(12)],
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    });
    assert!(
        !build.is_empty()
            && build.len() <= 128
            && build
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "invalid GRIDSTATOR_BUILD_ID"
    );
    println!("cargo:rustc-env=GRIDSTATOR_BUILD_ID={build}");
    println!("cargo:rustc-env=GRIDSTATOR_SOURCE_REVISION={revision}");
}
