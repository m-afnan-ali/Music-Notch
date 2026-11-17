//! Embeds the icon, manifest and version info. The version comes from
//! Cargo.toml, so bumping it there updates the exe details too.
use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc.in");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<String> =
        version.split(['.', '-', '+']).take(3).map(|p| p.parse::<u16>().unwrap_or(0).to_string()).collect();
    parts.resize(4, "0".into());

    // Resource scripts need absolute paths with escaped backslashes.
    let root = env::var("CARGO_MANIFEST_DIR").unwrap();
    let asset = |name: &str| Path::new(&root).join("assets").join(name).display().to_string().replace('\\', "\\\\");
    let rc = fs::read_to_string("assets/app.rc.in")
        .unwrap()
        .replace("@ICON@", &asset("icon.ico"))
        .replace("@MANIFEST@", &asset("app.manifest"))
        .replace("@VER_COMMAS@", &parts.join(","))
        .replace("@VER@", &version);

    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("app.rc");
    fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}
