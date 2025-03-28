use std::env::VarError;
use std::{env, path::PathBuf};

fn main() -> Result<(), VarError> {
    if cfg!(windows) {
        println!("cargo:rustc-link-lib=dylib=ze_loader_1");
        let env = env::var("CARGO_CFG_TARGET_ENV")?;
        if env == "msvc" {
            let mut path = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
            path.push("lib");
            println!("cargo:rustc-link-search=native={}", path.display());
        } else {
            println!("cargo:rustc-link-search=native=C:\\Windows\\System32");
        };
    } else {
        println!("cargo:rustc-link-lib=dylib=ze_loader");
        println!("cargo:rustc-link-search=native=/usr/lib/x86_64-linux-gnu/");
    }
    Ok(())
}
