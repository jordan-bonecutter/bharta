use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub search: String,
}
fn parse(text: &str, path: PathBuf, desktops: &[&str]) -> Option<Entry> {
    let mut in_entry = false;
    let mut fields = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if in_entry
            && !line.starts_with('#')
            && let Some((k, v)) = line.split_once('=')
        {
            fields.insert(k, v);
        }
    }
    if fields.get("Type") != Some(&"Application")
        || ["Hidden", "NoDisplay"]
            .iter()
            .any(|k| fields.get(k) == Some(&"true"))
    {
        return None;
    }
    if fields
        .get("OnlyShowIn")
        .is_some_and(|s| !s.split(';').any(|d| desktops.contains(&d)))
        || fields
            .get("NotShowIn")
            .is_some_and(|s| s.split(';').any(|d| desktops.contains(&d)))
    {
        return None;
    }
    if !fields.contains_key("Exec") && fields.get("DBusActivatable") != Some(&"true") {
        return None;
    }
    let name = fields
        .get("Name")?
        .replace("\\s", " ")
        .replace("\\\\", "\\");
    let search = format!(
        "{} {} {}",
        name,
        fields.get("GenericName").unwrap_or(&""),
        fields.get("Keywords").unwrap_or(&"")
    )
    .to_lowercase();
    Some(Entry { name, path, search })
}
pub fn entries() -> Vec<Entry> {
    let home = std::env::var("HOME").unwrap_or_default();
    let data_home =
        std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    let dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "sway".into());
    let desktops: Vec<_> = desktop.split(':').collect();
    let mut seen = HashSet::new();
    let mut result = vec![];
    for dir in std::iter::once(data_home.as_str()).chain(dirs.split(':')) {
        let root = Path::new(dir).join("applications");
        let mut files = vec![];
        collect(&root, &mut files, 0);
        files.sort();
        for path in files {
            let id = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('/', "-");
            if !seen.insert(id) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Some(entry) = parse(&text, path, &desktops)
            {
                result.push(entry);
            }
        }
    }
    result.sort_by_key(|e| e.name.to_lowercase());
    result
}
fn collect(dir: &Path, files: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let path = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                collect(&path, files, depth + 1);
            } else if path.extension().is_some_and(|x| x == "desktop") {
                files.push(path);
            }
        }
    }
}
pub fn launch(path: &Path) -> anyhow::Result<()> {
    // GIO implements Desktop Entry field codes, terminal apps and D-Bus activation.
    let status = std::process::Command::new("gio")
        .arg("launch")
        .arg(path)
        .status()?;
    anyhow::ensure!(status.success(), "Could not launch application");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn respects_visibility_and_desktop_groups() {
        let text = "[Desktop Entry]\nType=Application\nName=Editor\nExec=editor %U\nKeywords=code;\n[Desktop Action New]\nName=Wrong";
        assert_eq!(
            parse(text, "a.desktop".into(), &["sway"]).unwrap().name,
            "Editor"
        );
        assert!(
            parse(
                &text.replace("Keywords=code;", "Hidden=true"),
                "a.desktop".into(),
                &["sway"]
            )
            .is_none()
        );
        assert!(
            parse(
                &text.replace("Keywords=code;", "OnlyShowIn=GNOME;"),
                "a.desktop".into(),
                &["sway"]
            )
            .is_none()
        );
    }
}
