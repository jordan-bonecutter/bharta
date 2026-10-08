use std::{
    io::Read,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

const RATE: f64 = 24_000.0;
const WINDOW: usize = 1024;
const BANDS: [(f64, f64); 7] = [
    (60.0, 120.0),
    (120.0, 250.0),
    (250.0, 500.0),
    (500.0, 1_000.0),
    (1_000.0, 2_000.0),
    (2_000.0, 4_000.0),
    (4_000.0, 8_000.0),
];

pub struct Capture {
    child: Child,
    reader: Option<JoinHandle<()>>,
}
impl Capture {
    pub fn start(levels: Arc<Mutex<[f64; 7]>>) -> std::io::Result<Self> {
        let mut child = Command::new("parec")
            .args([
                "--device=@DEFAULT_MONITOR@",
                "--raw",
                "--format=s16le",
                "--rate=24000",
                "--channels=1",
                "--latency-msec=50",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("parec stdout is piped");
        let reader = thread::spawn(move || {
            let mut bytes = [0u8; WINDOW * 2];
            while stdout.read_exact(&mut bytes).is_ok() {
                let samples = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| i16::from_le_bytes(*pair) as f64 / 32768.0)
                    .collect::<Vec<_>>();
                if let Ok(mut output) = levels.lock() {
                    *output = analyze(&samples);
                }
            }
        });
        Ok(Self {
            child,
            reader: Some(reader),
        })
    }
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn analyze(samples: &[f64]) -> [f64; 7] {
    if samples.len() != WINDOW {
        return [0.0; 7];
    }
    let mut levels = [0.0; 7];
    for (index, (low, high)) in BANDS.iter().enumerate() {
        let first = (low * WINDOW as f64 / RATE).ceil() as usize;
        let last = (high * WINDOW as f64 / RATE).floor() as usize;
        let mut strongest: f64 = 0.0;
        for bin in first..=last {
            let omega = std::f64::consts::TAU * bin as f64 / WINDOW as f64;
            let coefficient = 2.0 * omega.cos();
            let (mut previous, mut before_previous) = (0.0, 0.0);
            for &sample in samples {
                let current = sample + coefficient * previous - before_previous;
                before_previous = previous;
                previous = current;
            }
            let power = previous.mul_add(
                previous,
                before_previous * before_previous - coefficient * previous * before_previous,
            );
            strongest = strongest.max(power.max(0.0).sqrt() / WINDOW as f64);
        }
        // Compress dynamic range so quiet frequency bands remain visible.
        levels[index] = (strongest * 8.0).clamp(0.0, 1.0).sqrt();
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tones_raise_only_their_matching_frequency_band() {
        let samples = (0..WINDOW)
            .map(|n| (std::f64::consts::TAU * 4.0 * n as f64 / WINDOW as f64).sin() * 0.5)
            .collect::<Vec<_>>();
        let levels = analyze(&samples);
        assert!(levels[0] > 0.5, "low tone was not measured: {levels:?}");
        assert!(levels[6] < 0.01, "high band leaked low tone: {levels:?}");
    }
    #[test]
    fn silence_has_no_equalizer_activity() {
        assert_eq!(analyze(&vec![0.0; WINDOW]), [0.0; 7]);
    }
}
