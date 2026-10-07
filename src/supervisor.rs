use anyhow::{Context, Result};
use std::{
    collections::{BTreeSet, HashMap},
    hash::{Hash, Hasher},
    os::unix::process::CommandExt,
    process::{Child, Command},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
#[derive(Default)]
struct Children(HashMap<String, Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for child in self.0.values_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn active_names(value: &serde_json::Value) -> Result<BTreeSet<String>> {
    Ok(value
        .as_array()
        .context("Invalid Sway output list")?
        .iter()
        .filter(|o| o["active"].as_bool() == Some(true))
        .filter_map(|o| o["name"].as_str().map(str::to_owned))
        .collect())
}
impl Children {
    fn reconcile(
        &mut self,
        outputs: &BTreeSet<String>,
        mut spawn: impl FnMut(&str) -> Result<Child>,
    ) -> Result<()> {
        self.0.retain(|name, child| {
            if !outputs.contains(name) {
                let _ = child.kill();
                let _ = child.wait();
                false
            } else {
                matches!(child.try_wait(), Ok(None))
            }
        });
        for name in outputs {
            if !self.0.contains_key(name) {
                self.0.insert(name.clone(), spawn(name)?);
            }
        }
        Ok(())
    }
}
pub fn run(options: &crate::Options) -> Result<()> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is missing")?;
    let socket = std::env::var("SWAYSOCK").context("Run bharta inside Sway")?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    socket.hash(&mut hash);
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(std::path::PathBuf::from(runtime).join(format!("bharta-{:x}.lock", hash.finish())))?;
    if lock.try_lock().is_err() {
        eprintln!("bharta is already managing this Sway session");
        return Ok(());
    }
    // Signal handlers only set an atomic flag; cleanup happens in the main loop.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let binary = std::env::current_exe()?;
    let parent = std::process::id() as libc::pid_t;
    let mut children = Children::default();
    let mut failures = 0;
    while !STOP.load(Ordering::Relaxed) {
        match crate::status::ipc(3, "").and_then(|v| active_names(&v)) {
            Ok(outputs) => {
                failures = 0;
                children.reconcile(&outputs, |name| {
                    let mut command = Command::new(&binary);
                    command.args(["--output", name]);
                    if options.dark {
                        command.arg("--dark");
                    }
                    if let Some(font) = &options.font {
                        command.args(["--font", font]);
                    }
                    // Prevent orphan bars even if the supervisor is killed abruptly.
                    unsafe {
                        command.pre_exec(move || {
                            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                                return Err(std::io::Error::last_os_error());
                            }
                            if libc::getppid() != parent {
                                return Err(std::io::Error::other("Supervisor exited"));
                            }
                            Ok(())
                        });
                    }
                    command
                        .spawn()
                        .with_context(|| format!("Start bar on {name}"))
                })?;
            }
            Err(error) => {
                failures += 1;
                if failures >= 5 {
                    return Err(error.context("Lost Sway connection"));
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn includes_all_active_outputs_even_when_powered_off() {
        let outputs = serde_json::json!([
            {"name":"DP-2","active":true},
            {"name":"eDP-1","active":true,"power":false},
            {"name":"HDMI-A-1","active":false}
        ]);
        assert_eq!(
            active_names(&outputs).unwrap(),
            BTreeSet::from(["DP-2".into(), "eDP-1".into()])
        );
        assert!(active_names(&serde_json::Value::Null).is_err());
    }
    #[test]
    fn hotplug_restarts_dead_children_without_duplicate_bars() {
        let mut children = Children::default();
        let mut count = 0;
        let mut spawn = |_: &str| {
            count += 1;
            Ok(Command::new("sleep").arg("60").spawn()?)
        };
        let both = BTreeSet::from(["DP-1".into(), "eDP-1".into()]);
        children.reconcile(&both, &mut spawn).unwrap();
        let original = children.0["eDP-1"].id();
        children.reconcile(&both, &mut spawn).unwrap();
        children.0.get_mut("DP-1").unwrap().kill().unwrap();
        children.0.get_mut("DP-1").unwrap().wait().unwrap();
        children.reconcile(&both, &mut spawn).unwrap();
        assert_eq!(children.0["eDP-1"].id(), original);
        children
            .reconcile(&BTreeSet::from(["eDP-1".into()]), &mut spawn)
            .unwrap();
        assert_eq!(children.0.len(), 1);
        children.reconcile(&both, &mut spawn).unwrap();
        assert_eq!(count, 4);
    }
}
