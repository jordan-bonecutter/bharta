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
    pub fn start(levels: Arc<Mutex<[f64; 7]>>, stream: u32) -> std::io::Result<Self> {
        Self::spawn(levels, Some(stream))
    }
    pub fn start_output(levels: Arc<Mutex<[f64; 7]>>) -> std::io::Result<Self> {
        Self::spawn(levels, None)
    }
    fn spawn(levels: Arc<Mutex<[f64; 7]>>, stream: Option<u32>) -> std::io::Result<Self> {
        let mut command = Command::new("parec");
        command.args([
            "--device=@DEFAULT_MONITOR@",
            "--raw",
            "--format=s16le",
            "--rate=24000",
            "--channels=1",
            "--latency-msec=50",
        ]);
        if let Some(stream) = stream {
            command.arg(format!("--monitor-stream={stream}"));
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("parec stdout is piped");
        let reader = thread::spawn(move || {
            let mut bytes = [0u8; WINDOW * 2];
            let mut samples = [0.0; WINDOW];
            let mut spectrum = Spectrum::new();
            while stdout.read_exact(&mut bytes).is_ok() {
                for (sample, pair) in samples.iter_mut().zip(bytes.as_chunks::<2>().0) {
                    *sample = i16::from_le_bytes(*pair) as f64 / 32768.0;
                }
                // Do the transform outside the lock so the UI never waits for analysis.
                let analyzed = spectrum.analyze(&samples);
                if let Ok(mut output) = levels.lock() {
                    *output = analyzed;
                }
            }
            if let Ok(mut output) = levels.lock() {
                *output = [0.; 7];
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
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

// One radix-2 FFT supplies every band, rather than scanning the entire sample
// window once per frequency bin. Reuse both scratch storage and twiddle factors.
struct Spectrum {
    values: [(f64, f64); WINDOW],
    roots: [(f64, f64); WINDOW / 2],
}
impl Spectrum {
    fn new() -> Self {
        Self {
            values: [(0.0, 0.0); WINDOW],
            roots: std::array::from_fn(|bin| {
                let angle = -std::f64::consts::TAU * bin as f64 / WINDOW as f64;
                let (sin, cos) = angle.sin_cos();
                (cos, sin)
            }),
        }
    }
    fn analyze(&mut self, samples: &[f64; WINDOW]) -> [f64; 7] {
        for (index, &sample) in samples.iter().enumerate() {
            let reversed = index.reverse_bits() >> (usize::BITS - WINDOW.ilog2());
            self.values[reversed] = (sample, 0.0);
        }
        let mut size = 2;
        while size <= WINDOW {
            let half = size / 2;
            for start in (0..WINDOW).step_by(size) {
                for offset in 0..half {
                    let (wr, wi) = self.roots[offset * WINDOW / size];
                    let (br, bi) = self.values[start + offset + half];
                    let (tr, ti) = (wr * br - wi * bi, wr * bi + wi * br);
                    let (ar, ai) = self.values[start + offset];
                    self.values[start + offset] = (ar + tr, ai + ti);
                    self.values[start + offset + half] = (ar - tr, ai - ti);
                }
            }
            size *= 2;
        }
        std::array::from_fn(|index| {
            let (low, high) = BANDS[index];
            let first = (low * WINDOW as f64 / RATE).ceil() as usize;
            let last = (high * WINDOW as f64 / RATE).floor() as usize;
            let strongest = self.values[first..=last]
                .iter()
                .map(|&(real, imaginary)| real.hypot(imaginary) / WINDOW as f64)
                .fold(0.0, f64::max);
            // Preserve the original band boundaries and dynamic-range compression.
            (strongest * 8.0).clamp(0.0, 1.0).sqrt()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference_analyze(samples: &[f64]) -> [f64; 7] {
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

    #[test]
    fn tones_raise_only_their_matching_frequency_band() {
        let samples = (0..WINDOW)
            .map(|n| (std::f64::consts::TAU * 4.0 * n as f64 / WINDOW as f64).sin() * 0.5)
            .collect::<Vec<_>>();
        let samples = samples.try_into().unwrap();
        let levels = Spectrum::new().analyze(&samples);
        assert!(levels[0] > 0.5, "low tone was not measured: {levels:?}");
        assert!(levels[6] < 0.01, "high band leaked low tone: {levels:?}");
    }
    #[test]
    fn silence_has_no_equalizer_activity() {
        assert_eq!(Spectrum::new().analyze(&[0.0; WINDOW]), [0.0; 7]);
    }
    #[test]
    fn fft_preserves_spectrum_for_tones_and_changing_audio() {
        let mut spectrum = Spectrum::new();
        for frequency in [
            0.0, 60.0, 93.75, 120.0, 250.0, 375.0, 750.0, 1600.0, 3000.0, 8000.0,
        ] {
            let samples = std::array::from_fn(|n| {
                (std::f64::consts::TAU * frequency * n as f64 / RATE).sin() * 0.03
            });
            assert_equivalent(&mut spectrum, &samples);
        }
        let mut seed = 42u32;
        for _ in 0..8 {
            let samples = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed as f64 / u32::MAX as f64 - 0.5) * 0.1
            });
            assert_equivalent(&mut spectrum, &samples);
        }
    }
    fn assert_equivalent(spectrum: &mut Spectrum, samples: &[f64; WINDOW]) {
        let expected = reference_analyze(samples);
        let actual = spectrum.analyze(samples);
        for (old, new) in expected.into_iter().zip(actual) {
            assert!(
                (old - new).abs() < 1e-6,
                "expected {expected:?}, got {actual:?}"
            );
        }
    }
    #[test]
    #[ignore = "manual release-mode spectrum benchmark"]
    fn spectrum_benchmark() {
        use std::{hint::black_box, time::Instant};
        let samples = std::array::from_fn(|n| (n as f64 * 0.137).sin() * 0.03);
        let start = Instant::now();
        for _ in 0..1000 {
            black_box(reference_analyze(black_box(&samples)));
        }
        let old = start.elapsed();
        let mut spectrum = Spectrum::new();
        let start = Instant::now();
        for _ in 0..1000 {
            black_box(spectrum.analyze(black_box(&samples)));
        }
        println!("1000 windows: original {old:?}, FFT {:?}", start.elapsed());
    }
}
