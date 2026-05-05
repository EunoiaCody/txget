use clap::CommandFactory;
use clap_complete::{generate_to, shells::Fish};
use std::env;
use std::path::PathBuf;

include!("src/lib.rs");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var_os("OUT_DIR").map(PathBuf::from).ok_or("OUT_DIR not set")?;
    let mut cmd = Args::command();
    let bin_name = "txget";

    // Generate fish completions
    let path = generate_to(Fish, &mut cmd, bin_name, &out_dir)?;

    let src = path.file_name().unwrap();
    let dst = dirs::config_dir()
        .map(|p| p.join("fish").join("completions").join(src))
        .unwrap_or_else(|| PathBuf::from("~/.config/fish/completions").join(src));

    
    if dst.parent().map(|p| std::fs::create_dir_all(p).is_ok()).unwrap_or(false) {
        if std::fs::copy(&path, &dst).is_ok() {
            println!("cargo:warning=Fish completions installed to {:?}", &dst);
        } else {
            println!(
                "cargo:warning=Fish completion: cp {:?} ~/.config/fish/completions/",
                &path
            );
        }
    } else {
        println!(
            "cargo:warning=Fish completion: cp {:?} ~/.config/fish/completions/",
            &path
        );
    }

    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=build.rs");

    Ok(())
}
