// Embeds app.ico into ro_launcher.exe so the launcher shows the project's icon
// in Explorer / the task bar instead of the default Rust icon.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("app.ico");
        // No requireAdministrator here: the launcher asks for elevation at
        // runtime (only Ragnarok.exe needs it), which keeps the UAC prompt
        // attached to the actual client start.
        res.set_manifest_file("app.manifest");
        if let Err(e) = res.compile() {
            println!("cargo:warning=winres: {e}");
        }
    }
    println!("cargo:rerun-if-changed=app.ico");
    println!("cargo:rerun-if-changed=app.manifest");
}