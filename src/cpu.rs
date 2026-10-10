use std::{collections::HashMap, fs, sync::mpsc, time::Instant};

#[derive(Clone, Default)]
pub struct Snapshot {
    pub percent: Option<f32>,
    pub processes: Vec<Process>,
}
#[derive(Clone)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    pub percent: f32,
    pub memory: u64,
}
#[derive(Default)]
struct Sample {
    total: u64,
    idle: u64,
    cores: u64,
    processes: HashMap<u32, (u64, u64)>,
}
fn process_stat(text: &str) -> Option<(String, u64, u64, u64)> {
    let (_, rest) = text.split_once('(')?;
    let (name, rest) = rest.rsplit_once(") ")?;
    let fields: Vec<_> = rest.split_whitespace().collect();
    Some((
        name.into(),
        fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?,
        fields.get(19)?.parse().ok()?,
        fields.get(21)?.parse::<i64>().ok()?.max(0) as u64,
    ))
}
fn sample(previous: &mut Sample, monitor: bool) -> Snapshot {
    let text = fs::read_to_string("/proc/stat").unwrap_or_default();
    let times: Vec<u64> = text
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .skip(1)
        .take(8)
        .filter_map(|n| n.parse().ok())
        .collect();
    let mut next = Sample {
        total: times.iter().sum(),
        idle: times.get(3).unwrap_or(&0) + times.get(4).unwrap_or(&0),
        cores: text
            .lines()
            .filter(|line| {
                line.strip_prefix("cpu")
                    .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_digit()))
            })
            .count() as u64,
        ..Default::default()
    };
    let elapsed = next.total.saturating_sub(previous.total);
    let mut result = Snapshot::default();
    if previous.total > 0 && elapsed > 0 {
        result.percent = Some(
            (100. * (elapsed.saturating_sub(next.idle.saturating_sub(previous.idle))) as f32
                / elapsed as f32)
                .clamp(0., 100.),
        );
    }
    if monitor {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
                    continue;
                };
                let Some((name, ticks, start, pages)) =
                    fs::read_to_string(entry.path().join("stat"))
                        .ok()
                        .and_then(|s| process_stat(&s))
                else {
                    continue;
                };
                let percent = previous
                    .processes
                    .get(&pid)
                    .filter(|(_, old_start)| *old_start == start)
                    .filter(|_| elapsed > 0)
                    .map(|(old, _)| {
                        100. * ticks.saturating_sub(*old) as f32 * next.cores as f32
                            / elapsed as f32
                    })
                    .unwrap_or(0.);
                next.processes.insert(pid, (ticks, start));
                result.processes.push(Process {
                    pid,
                    name,
                    percent,
                    memory: pages * page_size,
                });
            }
        }
        result.processes.sort_by(|a, b| {
            b.percent
                .total_cmp(&a.percent)
                .then(b.memory.cmp(&a.memory))
                .then(a.pid.cmp(&b.pid))
        });
        result
            .processes
            .truncate(crate::config::get().number("processes.rows") as usize);
    }
    *previous = next;
    result
}
pub fn watch() -> (mpsc::Sender<bool>, mpsc::Receiver<Snapshot>) {
    let (tx, requests) = mpsc::channel();
    let (updates, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut previous = Sample::default();
        let mut monitor = false;
        let mut first_scan = false;
        let mut percent = None;
        let mut next = Instant::now();
        loop {
            if Instant::now() >= next {
                let mut snapshot = sample(&mut previous, monitor);
                if first_scan {
                    snapshot.percent = percent;
                    first_scan = false;
                } else {
                    percent = snapshot.percent;
                }
                if let Err(mpsc::TrySendError::Disconnected(_)) = updates.try_send(snapshot) {
                    break;
                }
                next = Instant::now() + crate::config::get().duration("intervals.cpu_ms");
            }
            match requests.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(value) => {
                    if value && !monitor {
                        first_scan = true;
                        next = Instant::now();
                    }
                    monitor = value;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    });
    (tx, rx)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn monitor_samples_running_processes() {
        let mut previous = Sample::default();
        let snapshot = sample(&mut previous, true);
        assert!(!previous.processes.is_empty());
        assert!(previous.processes.contains_key(&std::process::id()));
        assert!(!snapshot.processes.is_empty());
    }
    #[test]
    fn opening_monitor_enables_process_updates() {
        let (requests, updates) = watch();
        let initial = updates.recv_timeout(Duration::from_secs(2)).unwrap();
        requests.send(true).unwrap();
        let snapshot = updates.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(snapshot.percent, initial.percent);
        assert!(!snapshot.processes.is_empty());
        assert!(
            updates
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .percent
                .is_some()
        );
    }
    #[test]
    fn process_names_with_spaces_and_parentheses_preserve_stat_fields() {
        let fields = "S 1 0 0 0 0 0 0 0 0 0 120 30 0 0 0 0 1 0 987 4096 42";
        assert_eq!(
            process_stat(&format!("123 (my (app)) {fields}")),
            Some(("my (app)".into(), 150, 987, 42))
        );
        assert!(process_stat("123 (gone) S").is_none());
    }
}
