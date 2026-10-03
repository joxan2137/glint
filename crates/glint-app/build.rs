use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let res = manifest_dir.join("res");
    for file in ["glint.rc", "glint.manifest", "glint.ico"] {
        println!("cargo:rerun-if-changed={}", res.join(file).display());
    }
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let part = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".into());
    let mut macros = vec![
        format!("GLINT_VERSION_MAJOR={}", part("CARGO_PKG_VERSION_MAJOR")),
        format!("GLINT_VERSION_MINOR={}", part("CARGO_PKG_VERSION_MINOR")),
        format!("GLINT_VERSION_PATCH={}", part("CARGO_PKG_VERSION_PATCH")),
        format!("GLINT_VERSION_STRING=\"{version}\""),
    ];
    if res.join("glint.ico").exists() {
        macros.push("GLINT_HAS_ICON".into());
    } else {
        println!("cargo:warning=res/glint.ico is missing; run `cargo run -p glint-app --example make_icon`");
    }
    let include_dirs = [res.clone()];
    embed_resource::compile(res.join("glint.rc"), embed_resource::ParamsMacrosAndIncludeDirs(&macros, &include_dirs))
        .manifest_required()
        .expect("compiling the Windows resources (manifest, icon, version info)");
}
