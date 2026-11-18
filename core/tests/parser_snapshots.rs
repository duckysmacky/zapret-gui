//! Parser snapshots over strategy files from several upstream releases (UPSTREAM.md: mandatory).

use std::path::Path;

use zapret_core::GameFilterMode;
use zapret_core::runner::{LaunchSettings, substitute_args};
use zapret_core::upstream::bundle::strategy_files;
use zapret_core::upstream::parser::parse_strategy;

#[test]
fn upstream_strategies() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for version in ["1.9.9b", "1.10.0", "1.10.4"] {
        let files = strategy_files(&fixtures.join(version));
        assert_eq!(files.len(), 4, "{version}");
        for (name, path) in files {
            let text = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
            let strategy = parse_strategy(&name, &text);
            assert_eq!(strategy.parse_error, None, "{version}/{name}");
            let settings = LaunchSettings {
                game_filter: GameFilterMode::All,
                game_tcp_ports: "1024-65535",
                game_udp_ports: "1024-65535",
                fake_discord_udp: None,
                fake_game_udp: None,
            };
            let args = substitute_args(&strategy.args, r"C:\B\bin\", r"C:\R\lists\", &settings);
            assert!(
                args.iter().all(|arg| !arg.contains('%')),
                "{version}/{name}: unresolved variable"
            );
            insta::assert_debug_snapshot!(format!("{version}__{name}"), strategy.args);
        }
    }
}
