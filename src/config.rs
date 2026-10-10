//! Startup configuration. Defaults are shared with the documented example.
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{path::PathBuf, sync::OnceLock, time::Duration};

pub const DEFAULT: &str = include_str!("../config.default.json");
static CONFIG: OnceLock<Config> = OnceLock::new();
pub struct Config(Value);
impl Default for Config {
    fn default() -> Self {
        Self(serde_json::from_str(DEFAULT).expect("valid bundled configuration"))
    }
}
impl Config {
    fn parse(text: &str) -> Result<Self> {
        let mut config = Self::default();
        merge(&mut config.0, &serde_json::from_str(text)?, "")?;
        Ok(config)
    }
    fn value(&self, key: &str) -> &Value {
        key.split('.').fold(&self.0, |value, part| &value[part])
    }
    pub fn number(&self, key: &str) -> f32 {
        self.value(key).as_f64().expect("validated numeric setting") as f32
    }
    pub fn duration(&self, key: &str) -> Duration {
        Duration::from_millis(self.number(key) as u64)
    }
    pub fn text(&self, key: &str) -> &str {
        self.value(key).as_str().expect("validated string setting")
    }
    pub fn dark(&self) -> bool {
        self.value("appearance.dark").as_bool().unwrap()
    }
    pub fn color(&self, role: &str, dark: bool) -> egui::Color32 {
        let key = format!("colors.{}.{role}", if dark { "dark" } else { "light" });
        let value = u32::from_str_radix(&self.text(&key)[1..], 16).unwrap();
        egui::Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }
}
fn merge(default: &mut Value, override_value: &Value, key: &str) -> Result<()> {
    match (default, override_value) {
        (Value::Object(default), Value::Object(values)) => {
            for (name, value) in values {
                let path = if key.is_empty() {
                    name.clone()
                } else {
                    format!("{key}.{name}")
                };
                let target = default
                    .get_mut(name)
                    .with_context(|| format!("Unknown setting {path}"))?;
                merge(target, value, &path)?;
            }
        }
        (target @ Value::Number(_), Value::Number(value)) => {
            let number = value.as_f64().context("Invalid number")?;
            let fractional = key.ends_with("_fraction") || key.ends_with("_scale");
            let maximum = if fractional {
                1.
            } else if key == "layout.popup_padding" {
                64.
            } else if key == "layout.bar_height" {
                200.
            } else if key.ends_with("_ms") {
                600_000.
            } else {
                4096.
            };
            ensure!(
                number.is_finite() && number > 0. && number <= maximum,
                "{key} must be greater than zero and at most {maximum}"
            );
            if key.ends_with("_ms") || key == "processes.rows" || key == "layout.bar_height" {
                ensure!(number.fract() == 0., "{key} must be an integer");
            }
            ensure!(
                key != "layout.bar_height" || number >= 20.,
                "layout.bar_height must be at least 20"
            );
            *target = Value::Number(value.clone());
        }
        (target @ Value::String(_), Value::String(value)) => {
            if key == "appearance.clock_format" {
                ensure!(
                    !chrono::format::StrftimeItems::new(value)
                        .any(|i| matches!(i, chrono::format::Item::Error)),
                    "appearance.clock_format is invalid"
                );
            }
            if key.starts_with("colors.") || key.contains("icon_color") {
                ensure!(
                    value.len() == 7
                        && value.starts_with('#')
                        && u32::from_str_radix(&value[1..], 16).is_ok(),
                    "{key} must be a #RRGGBB color"
                );
            }
            *target = Value::String(value.clone());
        }
        (target @ Value::Bool(_), Value::Bool(value)) => *target = Value::Bool(*value),
        _ => bail!("Wrong value type for {key}"),
    }
    Ok(())
}
pub fn path() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(root).join("bharta/config.json"));
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME is unset; use --config PATH")?)
            .join(".config/bharta/config.json"),
    )
}
pub fn init(path: Option<PathBuf>) -> Result<()> {
    let path = path.map(Ok).unwrap_or_else(self::path)?;
    let config = match std::fs::read_to_string(&path) {
        Ok(text) => Config::parse(&text)
            .with_context(|| format!("Invalid configuration {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(e).with_context(|| format!("Read configuration {}", path.display())),
    };
    CONFIG
        .set(config)
        .map_err(|_| anyhow::anyhow!("Configuration already initialized"))
}
pub fn get() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_example_passes_validation() {
        assert!(Config::parse(DEFAULT).is_ok());
    }
    #[test]
    fn partial_overrides_keep_defaults() {
        let c = Config::parse(
            r##"{"animation":{"preview_fade_ms":90},"colors":{"dark":{"bar":"#123456"}}}"##,
        )
        .unwrap();
        assert_eq!(
            c.duration("animation.preview_fade_ms"),
            Duration::from_millis(90)
        );
        assert_eq!(
            c.duration("animation.dismiss_delay_ms"),
            Duration::from_millis(500)
        );
        assert_eq!(c.color("bar", true), egui::Color32::from_rgb(18, 52, 86));
    }
    #[test]
    fn rejects_typos_and_unsafe_values() {
        for input in [
            r#"{"animation":{"typo_ms":1}}"#,
            r#"{"animation":{"frame_ms":0}}"#,
            r#"{"animation":{"frame_ms":1.5}}"#,
            r#"{"colors":{"dark":{"bar":"red"}}}"#,
            r#"{"preview":{"width_fraction":2}}"#,
            r#"{"appearance":{"dark":1}}"#,
        ] {
            assert!(Config::parse(input).is_err(), "accepted {input}");
        }
    }
}
