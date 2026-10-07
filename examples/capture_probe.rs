//! Capture a Sway toplevel by its foreign_toplevel_identifier without changing focus.
#[path = "../src/capture.rs"]
mod capture;
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() == 2,
        "Usage: capture_probe IDENTIFIER OUTPUT.png"
    );
    let mut capture = capture::Capture::new()?;
    let pix = capture.window(&args[0])?;
    pix.save_png(&args[1])?;
    println!("Captured {}×{} window thumbnail", pix.width(), pix.height());
    Ok(())
}
