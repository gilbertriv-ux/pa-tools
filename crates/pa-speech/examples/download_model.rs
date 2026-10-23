//! Fetch the pinned default model: `cargo run -p soundcraft-pa-speech --features download --example download_model -- <model-store-root>`.

#[cfg(feature = "download")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use soundcraft_pa_speech::models;
    let root = std::env::args_os().nth(1).ok_or("usage: download_model <model-store-root>")?;
    let model = models::find(models::DEFAULT_MODEL).ok_or("default model missing from catalogue")?;
    models::download(std::path::Path::new(&root), model, &mut |done, total, file| {
        eprint!("\r{file}: {done}/{total} bytes          ");
        true
    })?;
    eprintln!("\nModel files verified");
    Ok(())
}

#[cfg(not(feature = "download"))]
fn main() {
    eprintln!("This example requires --features download");
    std::process::exit(2);
}
