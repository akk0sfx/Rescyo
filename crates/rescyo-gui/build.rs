//! Writes the design tokens next to the .slint sources and compiles them.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(out.join("tokens.slint"), rescyo_design::SLINT_TOKENS)?;
    let config = slint_build::CompilerConfiguration::new()
        .with_include_paths(vec![out.clone()])
        .with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config)?;
    println!("cargo::rerun-if-changed=ui");
    println!("cargo::rerun-if-changed=../../assets/fonts");
    Ok(())
}
