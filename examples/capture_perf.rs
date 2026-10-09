//! Concurrent live/static preview benchmark, restricted to the headless harness.
#[path = "../src/capture.rs"]
mod capture;
fn main() -> anyhow::Result<()> {
    assert_eq!(std::env::var("BHARTA_HEADLESS").as_deref(), Ok("1"));
    let ids: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(ids.len() == 2, "Usage: capture_perf ANIMATED_ID STATIC_ID");
    let mut capture = capture::Capture::new()?;
    let targets: Vec<_> = ids.iter().map(|id| (id.clone(), 312, 220)).collect();
    let start = std::time::Instant::now();
    let mut counts = [0, 0];
    while start.elapsed() < std::time::Duration::from_secs(3) {
        for (id, _) in capture.poll(&targets)? {
            if start.elapsed().as_secs_f32() >= 1. {
                counts[usize::from(id != ids[0])] += 1;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    println!(
        "Animated preview: {:.1} FPS; static frames after warmup: {}",
        counts[0] as f32 / 2.,
        counts[1]
    );
    anyhow::ensure!(counts[0] >= 90, "Animating source failed to sustain 45 FPS");
    anyhow::ensure!(
        counts[1] <= 2,
        "Static source was recaptured without damage"
    );
    Ok(())
}
