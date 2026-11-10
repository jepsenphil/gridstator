fn main() {
    for variable in [
        "GRIDSTATOR_NATIVE_INSTALL_TAG",
        "GRIDSTATOR_DESKTOP_BUILD_ID",
        "GRIDSTATOR_SOURCE_REVISION",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    #[cfg(feature = "gui")]
    tauri_build::build()
}
