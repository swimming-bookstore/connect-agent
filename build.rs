use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    }
    println!("cargo:rerun-if-changed=proto/plane.proto");
    tonic_prost_build::configure().compile_protos(&["proto/plane.proto"], &["proto"])?;
    build_ui()?;
    Ok(())
}

fn build_ui() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let ui_manifest = manifest.join("ui/Cargo.toml");
    println!("cargo:rerun-if-changed=ui/src");
    println!("cargo:rerun-if-changed=ui/Cargo.toml");

    let mut cmd = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.arg("build")
        .arg("--manifest-path")
        .arg(&ui_manifest)
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("--release")
        .env("CARGO_TARGET_DIR", manifest.join("ui/target"));
    for (k, _) in env::vars() {
        if k.starts_with("CARGO") && k != "CARGO" && k != "CARGO_HOME" && k != "CARGO_TERM_COLOR" {
            cmd.env_remove(&k);
        }
    }
    cmd.env_remove("RUSTC");
    cmd.env_remove("RUSTC_WRAPPER");
    cmd.env_remove("RUSTFLAGS");

    let status = cmd.status()?;
    if !status.success() {
        return Err("ui wasm build failed".into());
    }

    let wasm = manifest.join("ui/target/wasm32-unknown-unknown/release/demo_client_ui.wasm");
    let out = PathBuf::from(env::var("OUT_DIR")?).join("webui");
    std::fs::create_dir_all(&out)?;
    wasm_bindgen_cli_support::Bindgen::new()
        .input_path(&wasm)
        .web(true)?
        .debug(false)
        .generate(&out)?;
    Ok(())
}
