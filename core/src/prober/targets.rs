//! `utils/targets.txt` (upstream format) and the DPI checker suite.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetValue {
    Url(String),
    Ping(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub value: TargetValue,
    /// Matters for the core use case (Discord, YouTube). Auto-strategy requires these to pass.
    pub critical: bool,
}

impl Target {
    pub fn value_text(&self) -> String {
        match &self.value {
            TargetValue::Url(url) => url.clone(),
            TargetValue::Ping(host) => format!("PING:{host}"),
        }
    }
}

/// Upstream's built-in defaults when `targets.txt` is missing or empty.
const DEFAULT_TARGETS: &[(&str, &str)] = &[
    ("Discord Main", "https://discord.com"),
    ("Discord Gateway", "https://gateway.discord.gg"),
    ("Discord CDN", "https://cdn.discordapp.com"),
    ("Discord Updates", "https://updates.discord.com"),
    ("YouTube Web", "https://www.youtube.com"),
    ("YouTube Short", "https://youtu.be"),
    ("YouTube Image", "https://i.ytimg.com"),
    (
        "YouTube Video Redirect",
        "https://redirector.googlevideo.com",
    ),
    ("Google Main", "https://www.google.com"),
    ("Google Gstatic", "https://www.gstatic.com"),
    ("Cloudflare Web", "https://www.cloudflare.com"),
    ("Cloudflare CDN", "https://cdnjs.cloudflare.com"),
    ("Cloudflare DNS 1.1.1.1", "PING:1.1.1.1"),
    ("Cloudflare DNS 1.0.0.1", "PING:1.0.0.1"),
    ("Google DNS 8.8.8.8", "PING:8.8.8.8"),
    ("Google DNS 8.8.4.4", "PING:8.8.4.4"),
    ("Quad9 DNS 9.9.9.9", "PING:9.9.9.9"),
];

/// Critical targets by normalized name (letters/digits, lowercase), so upstream renames don't silently drop them.
const CRITICAL_NAMES: &[&str] = &[
    "discordmain",
    "discordgateway",
    "discordcdn",
    "youtubeweb",
    "youtubeimage",
    "youtubevideoredirect",
];

/// Parses upstream's `Name = "value"` lines (regex `^\s*(\w+)\s*=\s*"(.+)"\s*$`). Other lines are ignored.
pub fn parse_targets(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            let value = value.trim();
            let is_word =
                !name.is_empty() && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
            let value = value.strip_prefix('"')?.strip_suffix('"')?;
            (is_word && !value.is_empty()).then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

fn to_target(name: &str, value: &str) -> Option<Target> {
    let value = if let Some(host) = value.strip_prefix("PING:") {
        TargetValue::Ping(host.trim().to_string())
    } else if value.starts_with("http://") || value.starts_with("https://") {
        TargetValue::Url(value.to_string())
    } else {
        return None;
    };
    Some(Target {
        name: name.to_string(),
        value,
        critical: false,
    })
}

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Targets from `targets.txt` text, or upstream's defaults when it has none. Critical flags applied.
pub fn targets_from_text(text: Option<&str>) -> Vec<Target> {
    let parsed: Vec<Target> = text
        .map(parse_targets)
        .unwrap_or_default()
        .iter()
        .filter_map(|(name, value)| to_target(name, value))
        .collect();
    let mut targets = if parsed.is_empty() {
        DEFAULT_TARGETS
            .iter()
            .filter_map(|(name, value)| to_target(name, value))
            .collect()
    } else {
        parsed
    };
    for target in &mut targets {
        target.critical = CRITICAL_NAMES.contains(&normalized(&target.name).as_str());
    }
    if !targets.iter().any(|target| target.critical) {
        for target in &mut targets {
            target.critical = matches!(target.value, TargetValue::Url(_));
        }
    }
    targets
}

pub fn load_targets(bundle_dir: Option<&Path>) -> Vec<Target> {
    let text = bundle_dir
        .and_then(|dir| std::fs::read_to_string(dir.join("utils").join("targets.txt")).ok());
    targets_from_text(text.as_deref())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct DpiTarget {
    pub id: String,
    pub provider: String,
    pub host: String,
}

/// Parses `suite.v2.json`, dropping entries whose host isn't a plain hostname.
pub fn parse_dpi_suite(json: &str) -> Option<Vec<DpiTarget>> {
    let entries: Vec<DpiTarget> = serde_json::from_str(json).ok()?;
    let valid: Vec<DpiTarget> = entries
        .into_iter()
        .filter(|entry| {
            !entry.host.is_empty()
                && entry.host.len() <= 253
                && entry
                    .host
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
        })
        .collect();
    (!valid.is_empty()).then_some(valid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_format() {
        let text = "# comment\nDiscordMain           = \"https://discord.com\"\n  GoogleDNS8888 = \"PING:8.8.8.8\"\nbad line\nTwo Words = \"https://x\"\nEmpty = \"\"\nNoQuotes = https://y\n";
        let targets = targets_from_text(Some(text));
        assert_eq!(targets.len(), 2);
        assert_eq!(
            targets[0].value,
            TargetValue::Url("https://discord.com".into())
        );
        assert!(targets[0].critical);
        assert_eq!(targets[1].value, TargetValue::Ping("8.8.8.8".into()));
        assert!(!targets[1].critical);
    }

    #[test]
    fn empty_uses_defaults() {
        let targets = targets_from_text(Some("# nothing\n"));
        assert_eq!(targets.len(), DEFAULT_TARGETS.len());
        assert_eq!(targets.iter().filter(|target| target.critical).count(), 6);
        assert_eq!(targets_from_text(None).len(), DEFAULT_TARGETS.len());
    }

    #[test]
    fn no_known_critical_falls_back_to_urls() {
        let targets = targets_from_text(Some("A = \"https://a.example\"\nB = \"PING:1.1.1.1\"\n"));
        assert!(targets[0].critical);
        assert!(!targets[1].critical);
    }

    #[test]
    fn dpi_suite() {
        let json = r#"[{"id":"A","provider":"P","country":"x","host":"ok.example.com"},
                       {"id":"B","provider":"P","country":"x","host":"bad/host"}]"#;
        let suite = parse_dpi_suite(json).unwrap();
        assert_eq!(suite.len(), 1);
        assert_eq!(suite[0].host, "ok.example.com");
        assert!(parse_dpi_suite("nope").is_none());
    }
}
