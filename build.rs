use clap::CommandFactory;
use clap_complete::{generate_to, shells::Fish};
use std::env;
use std::fs;
use std::path::PathBuf;

include!("src/lib.rs");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var_os("OUT_DIR").map(PathBuf::from).ok_or("OUT_DIR not set")?;
    
    // Create a completions directory in the workspace root or target
    // But for a simple build.rs, writing to OUT_DIR is the standard way.
    // If the user wants it in a specific place, we can adjust.
    // Let's also output to a 'completions' folder in the project root for visibility if possible, 
    // or just follow standard practice.
    
    let mut cmd = Args::command();
    let bin_name = "txget";

    // Generate fish completions
    let path = generate_to(
        Fish,
        &mut cmd,
        bin_name,
        &out_dir,
    )?;

    println!("cargo:warning=completion file is generated: {:?}", path);
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=build.rs");

    Ok(())
}
