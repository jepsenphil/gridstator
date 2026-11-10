//! cargo run --locked --example assemble_release -- INPUT OUTPUT [--require-notarization]
fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.len() == 2 || (args.len() == 3 && args[2] == "--require-notarization"),
        "usage: assemble_release INPUT OUTPUT [--require-notarization]"
    );
    let manifest = gridstator::packaging::assemble(
        std::path::Path::new(&args[0]),
        std::path::Path::new(&args[1]),
        args.len() == 3,
    )?;
    println!(
        "Assembled {} ({}) for four targets",
        manifest.version, manifest.build_id
    );
    Ok(())
}
