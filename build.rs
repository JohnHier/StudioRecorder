// Embeds icon and version info into the Windows .exe files:
// res/app.rc for StudioRecorder, res/archief.rc for ArchiefRecorder.
fn main() {
    for f in ["res/app.rc", "res/app.ico", "res/archief.rc", "res/archief.ico"] {
        println!("cargo:rerun-if-changed={f}");
    }
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let windres = if target.contains("gnu") && !cfg!(windows) { "x86_64-w64-mingw32-windres" } else { "windres" };
    for (bin, rc, obj) in [("studiorecorder", "res/app.rc", "app_res.o"), ("archiefrecorder", "res/archief.rc", "archief_res.o")] {
        let out = out_dir.join(obj);
        let status = std::process::Command::new(windres)
            .args(["--input", rc, "--output-format=coff", "--output"])
            .arg(&out)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status();
        match status {
            Ok(s) if s.success() => println!("cargo:rustc-link-arg-bin={bin}={}", out.display()),
            _ => println!("cargo:warning=windres not available; {bin} built without icon"),
        }
    }
}
