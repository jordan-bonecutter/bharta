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
fn desktop_fields(text: &str) -> HashMap<&str, &str> {
    let mut active = false;
    let mut fields = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            active = line == "[Desktop Entry]";
        } else if active
            && !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            fields.insert(key, value);
        }
    }
    fields
}
fn unescape(value: &str) -> String {
    let mut chars = value.chars();
    let mut result = String::new();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            result.push(ch);
            continue;
        }
        match chars.next() {
            Some('s') => result.push(' '),
            Some('n') => result.push('\n'),
            Some('t') => result.push('\t'),
            Some('r') => result.push('\r'),
            Some('\\') => result.push('\\'),
            Some(ch) => {
                result.push('\\');
                result.push(ch);
            }
            None => result.push('\\'),
        }
    }
    result
}
fn exec_args(exec: &str, name: &str, icon: &str, path: &Path) -> anyhow::Result<Vec<String>> {
    let value = unescape(exec);
    let mut chars = value.chars().peekable();
    let mut quoted = false;
    let mut present = false;
    let mut word = String::new();
    let mut words = vec![];
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                quoted = !quoted;
                present = true;
            }
            '\\' => {
                let next = chars
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("Trailing escape in application command"))?;
                if quoted && !matches!(next, '"' | '\\' | '$' | '`') {
                    word.push('\\');
                }
                word.push(next);
                present = true;
            }
            ch if ch.is_whitespace() && !quoted => {
                if present {
                    words.push(std::mem::take(&mut word));
                    present = false;
                }
            }
            ch => {
                word.push(ch);
                present = true;
            }
        }
    }
    anyhow::ensure!(!quoted, "Unclosed quote in application command");
    if present {
        words.push(word);
    }
    let mut result = vec![];
    for word in words {
        if word == "%i" {
            if !icon.is_empty() {
                result.extend(["--icon".into(), icon.into()]);
            }
            continue;
        }
        let mut value = String::new();
        let mut chars = word.chars();
        let mut removed = false;
        while let Some(ch) = chars.next() {
            if ch != '%' {
                value.push(ch);
                continue;
            }
            match chars.next() {
                Some('%') => value.push('%'),
                Some('c') => value.push_str(name),
                Some('k') => value.push_str(&path.to_string_lossy()),
                Some('f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm') => removed = true,
                _ => anyhow::bail!("Unsupported application field code"),
            }
        }
        if !removed || !value.is_empty() {
            result.push(value);
        }
    }
    anyhow::ensure!(
        result.first().is_some_and(|s| !s.is_empty()),
        "Application has no executable"
    );
    Ok(result)
}
fn activate(path: &Path) -> anyhow::Result<()> {
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid desktop application ID"))?;
    let object = format!("/{}", id.replace('.', "/").replace('-', "_"));
    let connection = zbus::blocking::Connection::session()?;
    let app = zbus::blocking::Proxy::new(
        &connection,
        id,
        object.as_str(),
        "org.freedesktop.Application",
    )?;
    let data: HashMap<String, zbus::zvariant::OwnedValue> = HashMap::new();
    let _: () = app.call("Activate", &(data,))?;
    Ok(())
}
fn terminal(args: Vec<String>) -> anyhow::Result<Vec<String>> {
    // Terminals are applications, not wrapper utilities. Use an installed desktop
    // terminal directly for Terminal=true entries, or the user's chosen terminal.
    if let Ok(command) = std::env::var("TERMINAL")
        && !command.is_empty()
    {
        let mut terminal = exec_args(&command, "", "", Path::new(""))?;
        terminal.push("-e".into());
        terminal.extend(args);
        return Ok(terminal);
    }
    for (program, flag) in [
        ("foot", "-e"),
        ("kitty", "--"),
        ("alacritty", "-e"),
        ("gnome-terminal", "--"),
        ("konsole", "-e"),
        ("xterm", "-e"),
    ] {
        if std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .any(|dir| dir.join(program).is_file())
        {
            let mut terminal = vec![program.into(), flag.into()];
            terminal.extend(args);
            return Ok(terminal);
        }
    }
    anyhow::bail!("No terminal application available for this entry")
}
pub fn launch(path: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let fields = desktop_fields(&text);
    if fields.get("DBusActivatable") == Some(&"true") {
        match activate(path) {
            Ok(()) => return Ok(()),
            Err(error) if !fields.contains_key("Exec") => return Err(error),
            Err(_) => (),
        }
    }
    let exec = fields
        .get("Exec")
        .ok_or_else(|| anyhow::anyhow!("Application has no launch command"))?;
    let mut args = exec_args(
        exec,
        &unescape(fields.get("Name").unwrap_or(&"")),
        &unescape(fields.get("Icon").unwrap_or(&"")),
        path,
    )?;
    if fields.get("Terminal") == Some(&"true") {
        args = terminal(args)?;
    }
    let mut command = std::process::Command::new(&args[0]);
    command.args(&args[1..]);
    if let Some(dir) = fields.get("Path")
        && !dir.is_empty()
    {
        command.current_dir(unescape(dir));
    }
    let mut child = command.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
/// Start an external launcher without waiting for its window to close.
pub fn launch_executable(executable: &str, args: &[&str]) -> std::io::Result<()> {
    let mut child = std::process::Command::new(executable).args(args).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
/// Resolve desktop metadata once per application, using the same XDG entries as Apps.
pub fn icon_name(application: &str) -> Option<String> {
    entries().into_iter().find_map(|entry| {
        let text = std::fs::read_to_string(&entry.path).ok()?;
        let mut section = false;
        let mut icon = None;
        let mut matches = entry
            .path
            .file_stem()
            .is_some_and(|id| id.to_string_lossy().eq_ignore_ascii_case(application))
            || entry.name.eq_ignore_ascii_case(application);
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line == "[Desktop Entry]";
            }
            if !section {
                continue;
            }
            if let Some(value) = line.strip_prefix("StartupWMClass=") {
                matches |= value.eq_ignore_ascii_case(application);
            }
            if let Some(value) = line.strip_prefix("Icon=") {
                icon = Some(value.to_owned());
            }
        }
        matches.then_some(icon).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exec_field_codes_and_quoting_preserve_literal_arguments() {
        let args = exec_args(
            r#"editor "a b" "$(literal)" %c %i %k %U %%"#,
            "My App",
            "app-icon",
            Path::new("/tmp/my app.desktop"),
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "editor",
                "a b",
                "$(literal)",
                "My App",
                "--icon",
                "app-icon",
                "/tmp/my app.desktop",
                "%"
            ]
        );
        assert!(exec_args("editor %q", "", "", Path::new("")).is_err());
        assert!(exec_args("editor \"unclosed", "", "", Path::new("")).is_err());
    }
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
