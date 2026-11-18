//! Building the final winws command line: strategy args + resolved paths + game filter + fakes.

use std::path::Path;

use crate::api::GameFilterMode;
use crate::paths::with_trailing_separator;
use crate::platform::WinwsLaunchSpec;
use crate::upstream::parser::Strategy;

/// Upstream's "effectively nothing" port.
pub const NO_PORTS: &str = "12";

/// Upstream's game filter port check: comma-separated `N` / `N-M`, 1..=65535, start ≤ end. Spaces are dropped.
pub fn validate_port_ranges(text: &str) -> Option<String> {
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    let port = |part: &str| -> Option<u32> {
        if part.is_empty()
            || part.len() > 5
            || part.starts_with('0')
            || !part.chars().all(|ch| ch.is_ascii_digit())
        {
            return None;
        }
        part.parse().ok().filter(|port| (1..=65535).contains(port))
    };
    for item in compact.split(',') {
        let (start, end) = match item.split_once('-') {
            Some((start, end)) => (port(start)?, port(end)?),
            None => {
                let single = port(item)?;
                (single, single)
            }
        };
        if start > end {
            return None;
        }
    }
    Some(compact)
}

/// (`%GameFilter%`, `%GameFilterTCP%`, `%GameFilterUDP%`) values, as upstream's `load_game_filter` sets them.
pub fn game_filter_ports(
    mode: GameFilterMode,
    tcp_ports: &str,
    udp_ports: &str,
) -> (String, String, String) {
    let (tcp, udp) = (tcp_ports.to_string(), udp_ports.to_string());
    match mode {
        GameFilterMode::Off => (NO_PORTS.into(), NO_PORTS.into(), NO_PORTS.into()),
        GameFilterMode::All => (tcp.clone(), tcp, udp),
        GameFilterMode::Tcp => (tcp.clone(), tcp, NO_PORTS.into()),
        GameFilterMode::Udp => (udp.clone(), NO_PORTS.into(), udp),
    }
}

pub struct LaunchSettings<'a> {
    pub game_filter: GameFilterMode,
    pub game_tcp_ports: &'a str,
    pub game_udp_ports: &'a str,
    /// Chosen fake names (without `.bin`), already checked to exist in the bundle.
    pub fake_discord_udp: Option<&'a str>,
    pub fake_game_udp: Option<&'a str>,
}

/// Substitutes `%BIN%`, `%LISTS%`, the game filter variables and the active fakes.
pub fn substitute_args(
    args: &[String],
    bin_dir: &str,
    lists_dir: &str,
    settings: &LaunchSettings,
) -> Vec<String> {
    let (game_any, game_tcp, game_udp) = game_filter_ports(
        settings.game_filter,
        settings.game_tcp_ports,
        settings.game_udp_ports,
    );
    args.iter()
        .map(|arg| {
            let mut arg = arg.clone();
            if let Some(fake) = settings.fake_discord_udp {
                arg = arg.replace("%BIN%ACTIVE_DISCORD_UDP.bin", &format!("%BIN%{fake}.bin"));
            }
            if let Some(fake) = settings.fake_game_udp {
                arg = arg.replace("%BIN%ACTIVE_GAME_UDP.bin", &format!("%BIN%{fake}.bin"));
            }
            arg.replace("%BIN%", bin_dir)
                .replace("%LISTS%", lists_dir)
                .replace("%GameFilterTCP%", &game_tcp)
                .replace("%GameFilterUDP%", &game_udp)
                .replace("%GameFilter%", &game_any)
        })
        .collect()
}

pub fn launch_spec(
    strategy: &Strategy,
    bundle_dir: &Path,
    runtime_lists_dir: &Path,
    settings: &LaunchSettings,
) -> WinwsLaunchSpec {
    let bin_dir = bundle_dir.join("bin");
    WinwsLaunchSpec {
        executable: bin_dir.join("winws.exe"),
        args: substitute_args(
            &strategy.args,
            &with_trailing_separator(&bin_dir),
            &with_trailing_separator(runtime_lists_dir),
            settings,
        ),
        working_dir: bin_dir,
        strategy_name: strategy.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_validation() {
        assert_eq!(
            validate_port_ranges("1024-65535").as_deref(),
            Some("1024-65535")
        );
        assert_eq!(
            validate_port_ranges(" 1024-1934, 1936-65535 ").as_deref(),
            Some("1024-1934,1936-65535")
        );
        for bad in ["", "0", "65536", "10-5", "a", "1--2", "1,", "01", "1-2-3"] {
            assert_eq!(validate_port_ranges(bad), None, "{bad}");
        }
    }

    #[test]
    fn game_filter_modes() {
        let ports = |mode| game_filter_ports(mode, "1000-2000", "3000-4000");
        assert_eq!(
            ports(GameFilterMode::Off),
            ("12".into(), "12".into(), "12".into())
        );
        assert_eq!(
            ports(GameFilterMode::All),
            ("1000-2000".into(), "1000-2000".into(), "3000-4000".into())
        );
        assert_eq!(
            ports(GameFilterMode::Tcp),
            ("1000-2000".into(), "1000-2000".into(), "12".into())
        );
        assert_eq!(
            ports(GameFilterMode::Udp),
            ("3000-4000".into(), "12".into(), "3000-4000".into())
        );
    }

    #[test]
    fn substitution() {
        let args: Vec<String> = [
            "--wf-tcp=80,443,%GameFilterTCP%",
            "--wf-udp=443,%GameFilterUDP%",
            "--old=%GameFilter%",
            "--hostlist=%LISTS%list-general.txt",
            "--fake=%BIN%ACTIVE_DISCORD_UDP.bin",
            "--game-fake=%BIN%ACTIVE_GAME_UDP.bin",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
        let settings = LaunchSettings {
            game_filter: GameFilterMode::Tcp,
            game_tcp_ports: "1024-65535",
            game_udp_ports: "1024-65535",
            fake_discord_udp: Some("stun"),
            fake_game_udp: None,
        };
        let result = substitute_args(&args, "C:\\b\\bin\\", "C:\\r\\lists\\", &settings);
        assert_eq!(
            result,
            vec![
                "--wf-tcp=80,443,1024-65535",
                "--wf-udp=443,12",
                "--old=1024-65535",
                "--hostlist=C:\\r\\lists\\list-general.txt",
                "--fake=C:\\b\\bin\\stun.bin",
                "--game-fake=C:\\b\\bin\\ACTIVE_GAME_UDP.bin",
            ]
        );
    }
}
