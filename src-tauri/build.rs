fn main() {
    // Release builds ask for administrator rights at launch (ETW capture and the WireGuard
    // tunnel service both need them). Debug builds keep the default manifest so `tauri dev`
    // works from a normal terminal; the in-app "Relaunch as admin" button covers that case.
    let release = std::env::var("PROFILE").map(|p| p == "release").unwrap_or(false);
    let attrs = if release {
        tauri_build::Attributes::new().windows_attributes(
            tauri_build::WindowsAttributes::new().app_manifest(include_str!("app.manifest")),
        )
    } else {
        tauri_build::Attributes::new()
    };
    tauri_build::try_build(attrs).expect("failed to run tauri build script");
}
