//! User list validation (invariant 4), ipset mode and the runtime lists folder winws reads.

use std::io;
use std::net::IpAddr;
use std::path::Path;

use crate::api::{IpsetMode, UserListKind, UserLists};

/// Upstream's placeholders: user list files must never be empty.
pub const DOMAIN_PLACEHOLDER: &str = "domain.example.abc";
pub const IP_PLACEHOLDER: &str = "203.0.113.113/32";

pub fn file_name(kind: UserListKind) -> &'static str {
    match kind {
        UserListKind::General => "list-general-user.txt",
        UserListKind::Exclude => "list-exclude-user.txt",
        UserListKind::IpsetExclude => "ipset-exclude-user.txt",
    }
}

fn is_valid_domain(text: &str) -> bool {
    if text.len() > 253 || !text.contains('.') {
        return false;
    }
    text.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    })
}

fn is_valid_ip_or_cidr(text: &str) -> bool {
    let (address, prefix) = match text.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (text, None),
    };
    let Ok(address) = address.parse::<IpAddr>() else {
        return false;
    };
    match prefix {
        None => true,
        Some(prefix) => {
            let max = if address.is_ipv4() { 32 } else { 128 };
            prefix.parse::<u8>().is_ok_and(|bits| bits <= max) && !prefix.starts_with('+')
        }
    }
}

/// Normalizes one entry. Hostlists take domains; the ipset list takes IPs / CIDRs.
pub fn validate_entry(kind: UserListKind, entry: &str) -> Option<String> {
    let entry = entry.trim();
    if entry.is_empty()
        || entry
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return None;
    }
    match kind {
        UserListKind::General | UserListKind::Exclude => {
            let lower = entry.to_ascii_lowercase();
            is_valid_domain(&lower).then_some(lower)
        }
        UserListKind::IpsetExclude => {
            is_valid_ip_or_cidr(entry).then(|| entry.to_ascii_lowercase())
        }
    }
}

/// Validates, dedupes (keeping order) and drops blank / `#` comment lines. Returns (accepted, rejected).
pub fn validate_entries(kind: UserListKind, entries: &[String]) -> (Vec<String>, Vec<String>) {
    let mut accepted: Vec<String> = Vec::new();
    let mut rejected = Vec::new();
    for raw in entries.iter().flat_map(|entry| entry.split(['\n', '\r'])) {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match validate_entry(kind, trimmed) {
            Some(entry) if !accepted.contains(&entry) => accepted.push(entry),
            Some(_) => {}
            None => rejected.push(trimmed.to_string()),
        }
    }
    (accepted, rejected)
}

/// Upstream's rule: empty = any, contains the placeholder = none, else loaded.
pub fn detect_ipset_mode(content: &str) -> IpsetMode {
    if content.lines().all(|line| line.trim().is_empty()) {
        IpsetMode::Any
    } else if content.contains(IP_PLACEHOLDER) {
        IpsetMode::None
    } else {
        IpsetMode::Loaded
    }
}

/// The real IP list for `loaded` mode: the downloaded override, else the bundle's list (or its `.backup`
/// when the bundle ships a switched-off list).
fn loaded_ipset(bundle_lists: &Path, ipset_override: Option<&Path>) -> io::Result<String> {
    if let Some(path) = ipset_override
        && let Ok(content) = std::fs::read_to_string(path)
        && detect_ipset_mode(&content) == IpsetMode::Loaded
    {
        return Ok(content);
    }
    let current = std::fs::read_to_string(bundle_lists.join("ipset-all.txt")).unwrap_or_default();
    if detect_ipset_mode(&current) == IpsetMode::Loaded {
        return Ok(current);
    }
    let backup =
        std::fs::read_to_string(bundle_lists.join("ipset-all.txt.backup")).unwrap_or_default();
    if detect_ipset_mode(&backup) == IpsetMode::Loaded {
        return Ok(backup);
    }
    Ok(current)
}

pub fn ipset_content(
    mode: IpsetMode,
    bundle_lists: &Path,
    ipset_override: Option<&Path>,
) -> io::Result<String> {
    match mode {
        IpsetMode::Loaded => loaded_ipset(bundle_lists, ipset_override),
        IpsetMode::None => Ok(format!("{IP_PLACEHOLDER}\n")),
        IpsetMode::Any => Ok(String::new()),
    }
}

fn user_list_content(kind: UserListKind, entries: &[String]) -> String {
    if entries.is_empty() {
        let placeholder = match kind {
            UserListKind::IpsetExclude => IP_PLACEHOLDER,
            _ => DOMAIN_PLACEHOLDER,
        };
        return format!("{placeholder}\n");
    }
    let mut content = entries.join("\n");
    content.push('\n');
    content
}

/// Rebuilds the runtime lists folder: bundle lists + user lists + ipset mode applied.
pub fn materialize(
    bundle_lists: &Path,
    runtime_dir: &Path,
    user_lists: &UserLists,
    ipset_mode: IpsetMode,
    ipset_override: Option<&Path>,
) -> io::Result<()> {
    if runtime_dir.exists() {
        std::fs::remove_dir_all(runtime_dir)?;
    }
    std::fs::create_dir_all(runtime_dir)?;
    for entry in std::fs::read_dir(bundle_lists)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_file() && !name.ends_with(".backup") {
            std::fs::copy(&path, runtime_dir.join(name))?;
        }
    }
    for kind in [
        UserListKind::General,
        UserListKind::Exclude,
        UserListKind::IpsetExclude,
    ] {
        std::fs::write(
            runtime_dir.join(file_name(kind)),
            user_list_content(kind, user_lists.get(kind)),
        )?;
    }
    std::fs::write(
        runtime_dir.join("ipset-all.txt"),
        ipset_content(ipset_mode, bundle_lists, ipset_override)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_validation() {
        let entries: Vec<String> = [
            " Example.COM ",
            "example.com",
            "sub.example.co.uk\nother.org",
            "# comment",
            "",
            "bad domain.com",
            "--opt=evil",
            "no-dot",
            "-bad.com",
            "a\u{7}.com",
            "пример.рф",
        ]
        .iter()
        .map(|entry| entry.to_string())
        .collect();
        let (accepted, rejected) = validate_entries(UserListKind::General, &entries);
        assert_eq!(
            accepted,
            vec!["example.com", "sub.example.co.uk", "other.org"]
        );
        assert_eq!(
            rejected,
            vec![
                "bad domain.com",
                "--opt=evil",
                "no-dot",
                "-bad.com",
                "a\u{7}.com",
                "пример.рф"
            ]
        );
    }

    #[test]
    fn ip_validation() {
        for good in [
            "1.2.3.4",
            "10.0.0.0/8",
            "203.0.113.113/32",
            "2001:db8::/32",
            "::1",
        ] {
            assert!(
                validate_entry(UserListKind::IpsetExclude, good).is_some(),
                "{good}"
            );
        }
        for bad in [
            "1.2.3.4/33",
            "1.2.3",
            "example.com",
            "1.2.3.4/",
            "1.2.3.4/+8",
            "::/129",
        ] {
            assert!(
                validate_entry(UserListKind::IpsetExclude, bad).is_none(),
                "{bad}"
            );
        }
    }

    #[test]
    fn ipset_modes() {
        assert_eq!(detect_ipset_mode(""), IpsetMode::Any);
        assert_eq!(detect_ipset_mode("\n\n"), IpsetMode::Any);
        assert_eq!(detect_ipset_mode("203.0.113.113/32\n"), IpsetMode::None);
        assert_eq!(detect_ipset_mode("1.1.1.1/32\n"), IpsetMode::Loaded);
    }

    #[test]
    fn materializes_runtime_lists() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("lists");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("list-general.txt"), "discord.com\n").unwrap();
        std::fs::write(bundle.join("ipset-all.txt"), "203.0.113.113/32\n").unwrap();
        std::fs::write(bundle.join("ipset-all.txt.backup"), "1.1.1.0/24\n").unwrap();
        let runtime = dir.path().join("runtime");
        let user = UserLists {
            general: vec!["example.com".into()],
            ..Default::default()
        };

        materialize(&bundle, &runtime, &user, IpsetMode::Loaded, None).unwrap();
        let read = |name: &str| std::fs::read_to_string(runtime.join(name)).unwrap();
        assert_eq!(read("list-general.txt"), "discord.com\n");
        assert_eq!(read("list-general-user.txt"), "example.com\n");
        assert_eq!(read("list-exclude-user.txt"), "domain.example.abc\n");
        assert_eq!(read("ipset-exclude-user.txt"), "203.0.113.113/32\n");
        assert_eq!(read("ipset-all.txt"), "1.1.1.0/24\n");
        assert!(!runtime.join("ipset-all.txt.backup").exists());

        materialize(&bundle, &runtime, &user, IpsetMode::Any, None).unwrap();
        assert_eq!(read("ipset-all.txt"), "");
        materialize(&bundle, &runtime, &user, IpsetMode::None, None).unwrap();
        assert_eq!(read("ipset-all.txt"), "203.0.113.113/32\n");

        let override_file = dir.path().join("ipset-service.txt");
        std::fs::write(&override_file, "9.9.9.0/24\n").unwrap();
        materialize(
            &bundle,
            &runtime,
            &user,
            IpsetMode::Loaded,
            Some(&override_file),
        )
        .unwrap();
        assert_eq!(read("ipset-all.txt"), "9.9.9.0/24\n");
    }
}
