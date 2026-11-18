//! Upstream's `service.bat` diagnostics (checks 1-17, in upstream order) and their fixes.

use std::path::{Path, PathBuf};

use crate::api::{CheckResult, CheckStatus, FixKind};
use crate::platform::{PlatformError, ServiceInfo, System, SystemFacts};
use crate::upstream::bundle;

pub const CONFLICTING_SERVICES: &[&str] = &["GoodbyeDPI", "discordfix_zapret", "winws1", "winws2"];
/// Discord flavours: (folder under `%APPDATA%`, process image, label).
pub const DISCORD_INSTALLS: &[(&str, &str, &str)] = &[
    ("discord", "Discord.exe", "Discord"),
    ("discordptb", "DiscordPTB.exe", "Discord PTB"),
    ("discordcanary", "DiscordCanary.exe", "Discord Canary"),
    (
        "discorddevelopment",
        "DiscordDevelopment.exe",
        "Discord Development",
    ),
];
const DISCORD_CACHE_DIRS: &[&str] = &["Cache", "Code Cache", "GPUCache"];

/// Everything the checks look at, collected once (blocking).
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub services: Vec<ServiceInfo>,
    pub processes: Vec<String>,
    pub system: SystemFacts,
    pub hosts: Option<String>,
    pub bundle_dir: Option<PathBuf>,
    pub onedrive: Option<String>,
    pub discord_found: Vec<String>,
}

pub fn collect(system: &dyn System, bundle_dir: Option<&Path>) -> Facts {
    let services = system.services().unwrap_or_else(|error| {
        tracing::warn!("cannot list services: {error}");
        Vec::new()
    });
    let processes = system.process_names().unwrap_or_default();
    let hosts = std::fs::read(system.hosts_path())
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let discord_found = system
        .appdata_dir()
        .map(|appdata| {
            DISCORD_INSTALLS
                .iter()
                .filter(|(folder, _, _)| appdata.join(folder).is_dir())
                .map(|(_, _, label)| (*label).to_string())
                .collect()
        })
        .unwrap_or_default();
    Facts {
        services,
        processes,
        system: system.system_facts(),
        hosts,
        bundle_dir: bundle_dir.map(Path::to_path_buf),
        onedrive: std::env::var("OneDrive")
            .ok()
            .filter(|value| !value.is_empty()),
        discord_found,
    }
}

fn service_matches(service: &ServiceInfo, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    service.name.to_lowercase().contains(&needle)
        || service.display_name.to_lowercase().contains(&needle)
}

/// Running Win32 services (what `sc query` lists) matching every needle.
fn running_services<'a>(facts: &'a Facts, needles: &[&str]) -> Vec<&'a ServiceInfo> {
    facts
        .services
        .iter()
        .filter(|service| service.running && !service.is_driver)
        .filter(|service| {
            needles
                .iter()
                .all(|needle| service_matches(service, needle))
        })
        .collect()
}

fn installed<'a>(facts: &'a Facts, name: &str) -> Option<&'a ServiceInfo> {
    facts
        .services
        .iter()
        .find(|service| service.name.eq_ignore_ascii_case(name))
}

fn has_process(facts: &Facts, image: &str) -> bool {
    facts
        .processes
        .iter()
        .any(|process| process.eq_ignore_ascii_case(image))
}

fn check(
    number: u8,
    title: &str,
    status: CheckStatus,
    detail: impl Into<String>,
    fix: Option<FixKind>,
) -> CheckResult {
    CheckResult {
        number,
        title: title.into(),
        status,
        detail: detail.into(),
        fix,
    }
}

fn simple(number: u8, title: &str, problem: bool, problem_detail: &str) -> CheckResult {
    if problem {
        check(number, title, CheckStatus::Problem, problem_detail, None)
    } else {
        check(number, title, CheckStatus::Ok, "Не обнаружено", None)
    }
}

fn is_cyrillic(ch: char) -> bool {
    matches!(ch, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё')
}

pub fn evaluate(facts: &Facts) -> Vec<CheckResult> {
    let mut checks = Vec::new();

    let bfe_running = facts
        .services
        .iter()
        .any(|service| service.name.eq_ignore_ascii_case("BFE") && service.running);
    checks.push(if bfe_running {
        check(
            1,
            "Служба Base Filtering Engine",
            CheckStatus::Ok,
            "Работает",
            None,
        )
    } else {
        check(
            1,
            "Служба Base Filtering Engine",
            CheckStatus::Problem,
            "Служба BFE не запущена, без неё zapret не работает. Запустите её в services.msc.",
            None,
        )
    });

    checks.push(if facts.system.proxy_enabled {
        check(
            2,
            "Системный прокси",
            CheckStatus::Warning,
            format!(
                "Включён прокси {}. Убедитесь, что он нужен, или отключите его.",
                facts.system.proxy_server
            ),
            None,
        )
    } else {
        check(
            2,
            "Системный прокси",
            CheckStatus::Ok,
            "Не используется",
            None,
        )
    });

    checks.push(match facts.system.tcp_timestamps {
        Some(true) => check(3, "TCP timestamps", CheckStatus::Ok, "Включены", None),
        Some(false) => check(
            3,
            "TCP timestamps",
            CheckStatus::Warning,
            "Выключены. Их можно включить автоматически.",
            Some(FixKind::EnableTcpTimestamps),
        ),
        None => check(
            3,
            "TCP timestamps",
            CheckStatus::Warning,
            "Не удалось определить состояние.",
            Some(FixKind::EnableTcpTimestamps),
        ),
    });

    checks.push(simple(
        4,
        "Adguard",
        has_process(facts, "AdguardSvc.exe"),
        "Запущен Adguard, он может мешать Discord. Отключите или удалите его.",
    ));
    checks.push(simple(
        5,
        "Службы Killer",
        !running_services(facts, &["Killer"]).is_empty(),
        "Найдены службы Killer, они конфликтуют с zapret.",
    ));
    checks.push(simple(
        6,
        "Intel Connectivity Network Service",
        !running_services(facts, &["Intel", "Connectivity", "Network"]).is_empty(),
        "Найдена служба Intel Connectivity Network, она конфликтует с zapret.",
    ));
    checks.push(simple(
        7,
        "Check Point",
        !running_services(facts, &["TracSrvWrapper"]).is_empty()
            || !running_services(facts, &["EPWD"]).is_empty(),
        "Найдены службы Check Point, они конфликтуют с zapret. Попробуйте удалить Check Point.",
    ));
    checks.push(simple(
        8,
        "SmartByte",
        !running_services(facts, &["SmartByte"]).is_empty(),
        "Найдены службы SmartByte, они конфликтуют с zapret. Удалите или отключите их в services.msc.",
    ));

    let bundle_path = facts
        .bundle_dir
        .as_ref()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    checks.push(if bundle_path.chars().any(is_cyrillic) {
        check(
            9,
            "Кириллица в пути",
            CheckStatus::Warning,
            format!("Путь к файлам zapret содержит кириллицу: {bundle_path}"),
            None,
        )
    } else {
        check(
            9,
            "Кириллица в пути",
            CheckStatus::Ok,
            "Не обнаружено",
            None,
        )
    });

    let in_onedrive = facts.onedrive.as_ref().is_some_and(|onedrive| {
        !bundle_path.is_empty()
            && bundle_path
                .to_lowercase()
                .starts_with(&onedrive.to_lowercase())
    });
    checks.push(simple(
        10,
        "Папка OneDrive",
        in_onedrive,
        "Файлы zapret находятся в папке OneDrive. Это может мешать работе.",
    ));

    checks.push(match &facts.bundle_dir {
        Some(dir) if bundle::has_windivert_driver(dir) => check(
            11,
            "Файл WinDivert64.sys",
            CheckStatus::Ok,
            "На месте",
            None,
        ),
        Some(_) => check(
            11,
            "Файл WinDivert64.sys",
            CheckStatus::Problem,
            "Драйвер WinDivert не найден (возможно, его удалил антивирус). Скачайте версию заново.",
            Some(FixKind::RedownloadBundle),
        ),
        None => check(
            11,
            "Файл WinDivert64.sys",
            CheckStatus::Warning,
            "Версия zapret ещё не скачана.",
            None,
        ),
    });

    let mut vpn: Vec<&ServiceInfo> = running_services(facts, &["VPN"]);
    vpn.extend(running_services(facts, &["HappService"]));
    checks.push(if vpn.is_empty() {
        check(12, "VPN", CheckStatus::Ok, "Не обнаружено", None)
    } else {
        let names: Vec<&str> = vpn
            .iter()
            .map(|service| service.display_name.as_str())
            .collect();
        check(
            12,
            "VPN",
            CheckStatus::Warning,
            format!(
                "Найдены службы VPN: {}. Некоторые VPN конфликтуют с zapret, отключите их.",
                names.join(", ")
            ),
            None,
        )
    });

    checks.push(if facts.system.doh_configured {
        check(13, "Защищённый DNS", CheckStatus::Ok, "Настроен", None)
    } else {
        check(
            13,
            "Защищённый DNS",
            CheckStatus::Warning,
            "Настройте защищённый DNS (DNS-over-HTTPS) в браузере или в параметрах Windows 11.",
            None,
        )
    });

    let hosts_lower = facts.hosts.as_deref().unwrap_or_default().to_lowercase();
    checks.push(
        if hosts_lower.contains("youtube.com") || hosts_lower.contains("youtu.be") {
            check(
                14,
                "Файл hosts",
                CheckStatus::Warning,
                "В файле hosts есть записи youtube.com или youtu.be. Они могут мешать YouTube.",
                None,
            )
        } else {
            check(
                14,
                "Файл hosts",
                CheckStatus::Ok,
                "Записей YouTube нет",
                None,
            )
        },
    );

    let windivert_active = installed(facts, "WinDivert")
        .is_some_and(|service| service.running || service.stop_pending);
    checks.push(if windivert_active && !has_process(facts, "winws.exe") {
        check(
            15,
            "Конфликт WinDivert",
            CheckStatus::Warning,
            "WinDivert активен, хотя winws не запущен (старый драйвер или GoodbyeDPI).",
            Some(FixKind::RemoveWinDivert),
        )
    } else {
        check(
            15,
            "Конфликт WinDivert",
            CheckStatus::Ok,
            "Не обнаружено",
            None,
        )
    });

    let conflicts: Vec<&str> = CONFLICTING_SERVICES
        .iter()
        .copied()
        .filter(|name| installed(facts, name).is_some())
        .collect();
    checks.push(if conflicts.is_empty() {
        check(
            16,
            "Другие обходы блокировок",
            CheckStatus::Ok,
            "Не обнаружено",
            None,
        )
    } else {
        check(
            16,
            "Другие обходы блокировок",
            CheckStatus::Problem,
            format!("Найдены конфликтующие службы: {}", conflicts.join(", ")),
            Some(FixKind::RemoveConflictingServices),
        )
    });

    checks.push(if facts.discord_found.is_empty() {
        check(
            17,
            "Кэш Discord",
            CheckStatus::Ok,
            "Discord не найден",
            None,
        )
    } else {
        check(
            17,
            "Кэш Discord",
            CheckStatus::Ok,
            format!(
                "Найдено: {}. Очистка кэша иногда помогает, если Discord не грузится.",
                facts.discord_found.join(", ")
            ),
            Some(FixKind::ClearDiscordCache),
        )
    });

    checks
}

/// Fix 15: remove WinDivert; if that fails, remove GoodbyeDPI and retry (upstream behaviour).
pub fn remove_windivert(system: &dyn System) -> Result<(), PlatformError> {
    if system.remove_service("WinDivert").is_ok() && !service_exists(system, "WinDivert") {
        return Ok(());
    }
    system.remove_service("GoodbyeDPI")?;
    system.remove_service("WinDivert")?;
    if service_exists(system, "WinDivert") {
        return Err(PlatformError::Failed(
            "WinDivert не удаётся удалить. Проверьте вручную, какой обход его использует.".into(),
        ));
    }
    Ok(())
}

fn service_exists(system: &dyn System, name: &str) -> bool {
    system.services().is_ok_and(|services| {
        services
            .iter()
            .any(|service| service.name.eq_ignore_ascii_case(name))
    })
}

/// Fix 16: remove conflicting bypass services, then WinDivert / WinDivert14.
pub fn remove_conflicting_services(system: &dyn System) -> Result<(), PlatformError> {
    for name in CONFLICTING_SERVICES {
        if service_exists(system, name) {
            system.remove_service(name)?;
        }
    }
    let _ = system.remove_service("WinDivert");
    let _ = system.remove_service("WinDivert14");
    Ok(())
}

/// "Remove Services" parity: `zapret` service, any winws, `WinDivert`, `WinDivert14`.
pub fn remove_old_services(system: &dyn System) -> Result<(), PlatformError> {
    system.remove_service("zapret")?;
    system.kill_processes("winws.exe")?;
    system.remove_service("WinDivert")?;
    let _ = system.remove_service("WinDivert14");
    Ok(())
}

/// Closes Discord and deletes its caches. Returns a short report.
pub fn clear_discord_cache(system: &dyn System) -> Vec<String> {
    let Some(appdata) = system.appdata_dir() else {
        return vec!["Папка %APPDATA% не найдена".into()];
    };
    let mut report = Vec::new();
    for (folder, image, label) in DISCORD_INSTALLS {
        let dir = appdata.join(folder);
        if !dir.is_dir() {
            continue;
        }
        if let Ok(count) = system.kill_processes(image)
            && count > 0
        {
            report.push(format!("{label} закрыт"));
        }
        for cache in DISCORD_CACHE_DIRS {
            let cache_dir = dir.join(cache);
            if cache_dir.is_dir() {
                match std::fs::remove_dir_all(&cache_dir) {
                    Ok(()) => report.push(format!("Удалено: {}", cache_dir.display())),
                    Err(error) => report.push(format!(
                        "Не удалось удалить {}: {error}",
                        cache_dir.display()
                    )),
                }
            }
        }
    }
    if report.is_empty() {
        report.push("Кэш Discord не найден".into());
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str, running: bool) -> ServiceInfo {
        ServiceInfo {
            name: name.into(),
            display_name: name.into(),
            running,
            stop_pending: false,
            is_driver: name.starts_with("WinDivert"),
        }
    }

    #[test]
    fn healthy_system() {
        let facts = Facts {
            services: vec![service("BFE", true)],
            system: SystemFacts {
                doh_configured: true,
                tcp_timestamps: Some(true),
                ..Default::default()
            },
            ..Default::default()
        };
        let checks = evaluate(&facts);
        assert_eq!(checks.len(), 17);
        assert_eq!(
            checks.iter().map(|check| check.number).collect::<Vec<_>>(),
            (1..=17).collect::<Vec<_>>()
        );
        // Only "no bundle" is reported.
        let not_ok: Vec<u8> = checks
            .iter()
            .filter(|check| check.status != CheckStatus::Ok)
            .map(|check| check.number)
            .collect();
        assert_eq!(not_ok, vec![11]);
    }

    #[test]
    fn problems_are_found() {
        let facts = Facts {
            services: vec![
                service("Killer Network Service", true),
                service("NordVPN", true),
                service("GoodbyeDPI", false),
                service("WinDivert", true),
                service("SmartByte Stopped", false),
            ],
            processes: vec!["AdguardSvc.exe".into()],
            system: SystemFacts {
                proxy_enabled: true,
                proxy_server: "127.0.0.1:8080".into(),
                doh_configured: false,
                tcp_timestamps: Some(false),
            },
            hosts: Some("1.2.3.4 www.YouTube.com".into()),
            bundle_dir: Some(PathBuf::from("/data/бандл")),
            onedrive: None,
            discord_found: vec!["Discord".into()],
        };
        let checks = evaluate(&facts);
        let status = |number: u8| checks[usize::from(number) - 1].status;
        assert_eq!(status(1), CheckStatus::Problem);
        assert_eq!(status(2), CheckStatus::Warning);
        assert_eq!(checks[2].fix, Some(FixKind::EnableTcpTimestamps));
        assert_eq!(status(4), CheckStatus::Problem);
        assert_eq!(status(5), CheckStatus::Problem);
        assert_eq!(status(8), CheckStatus::Ok, "stopped services are ignored");
        assert_eq!(status(9), CheckStatus::Warning);
        assert_eq!(status(11), CheckStatus::Problem);
        assert_eq!(status(12), CheckStatus::Warning);
        assert_eq!(status(13), CheckStatus::Warning);
        assert_eq!(status(14), CheckStatus::Warning);
        assert_eq!(checks[14].fix, Some(FixKind::RemoveWinDivert));
        assert_eq!(checks[15].fix, Some(FixKind::RemoveConflictingServices));
        assert_eq!(checks[16].fix, Some(FixKind::ClearDiscordCache));
    }
}
