// Embeds the icon and version info into the Windows .exe.
fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.ico");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app_res.o");
    let windres = if target.contains("gnu") && !cfg!(windows) { "x86_64-w64-mingw32-windres" } else { "windres" };
    let status = std::process::Command::new(windres)
        .args(["--input", "res/app.rc", "--output-format=coff", "--output"])
        .arg(&out)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status();
    match status {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", out.display()),
        _ => println!("cargo:warning=windres not available; building without icon"),
    }
}
