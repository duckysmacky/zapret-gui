//! Real Windows implementations of the platform seams.

use std::ffi::c_void;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc;
use windows::Win32::Foundation::{CloseHandle, ERROR_SERVICE_DOES_NOT_EXIST, HANDLE};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
use windows::Win32::NetworkManagement::IpHelper::{
    ICMP_ECHO_REPLY, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, NotifyAddrChange,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, DeleteService, ENUM_SERVICE_STATUS_PROCESSW,
    EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_ENUM_PROCESS_INFO,
    SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_CONTROL_STOP, SERVICE_DRIVER,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATE_ALL, SERVICE_STATUS, SERVICE_STOP,
    SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_WIN32,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_TERMINATE, TerminateProcess,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

use super::{
    Platform, PlatformError, RunningWinws, ServiceInfo, System, SystemFacts, WinwsDriver,
    WinwsLaunchSpec,
};
use crate::paths::Paths;
use crate::prober::RealProbe;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Standard `DELETE` access right.
const ACCESS_DELETE: u32 = 0x0001_0000;
const TASK_NAME: &str = "zapret-gui";

pub fn create(paths: &Paths) -> Platform {
    let system: Arc<dyn System> = Arc::new(WindowsSystem {
        data_dir: paths.data_dir.clone(),
    });
    Platform {
        system: system.clone(),
        driver: Arc::new(WindowsDriver {
            system: system.clone(),
        }),
        probe: Arc::new(RealProbe::new(
            system,
            paths.cache_dir().join("dpi-suite.json"),
        )),
        is_mock: false,
        fail_downloads: false,
    }
}

fn failed(what: &str, error: impl std::fmt::Display) -> PlatformError {
    PlatformError::Failed(format!("{what}: {error}"))
}

/// Runs a system tool hidden, returning (success, stdout).
fn run_tool(program: &str, args: &[&str]) -> Result<(bool, String), PlatformError> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
}

fn system_tool(name: &str) -> String {
    system_root()
        .join("System32")
        .join(name)
        .display()
        .to_string()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub struct WindowsSystem {
    data_dir: PathBuf,
}

// ----- registry helpers ------------------------------------------------------------------------------------------

fn reg_dword(root: HKEY, path: &str, value: &str) -> Option<u32> {
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(path),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(data)
}

fn reg_string(root: HKEY, path: &str, value: &str) -> Option<String> {
    let path = HSTRING::from(path);
    let value = HSTRING::from(value);
    let mut size = 0u32;
    let probe = unsafe {
        RegGetValueW(
            root,
            &path,
            &value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if probe.is_err() || size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; size as usize / 2 + 1];
    let status = unsafe {
        RegGetValueW(
            root,
            &path,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let length = buffer
        .iter()
        .position(|ch| *ch == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// Upstream's Secure DNS check: any key under the Dnscache interface parameters with `DohFlags > 0`.
fn doh_configured() -> bool {
    fn walk(key: HKEY, depth: u32) -> bool {
        if reg_dword(key, "", "DohFlags").is_some_and(|flags| flags > 0) {
            return true;
        }
        if depth == 0 {
            return false;
        }
        let mut index = 0;
        loop {
            let mut name = [0u16; 256];
            let mut length = name.len() as u32;
            let status = unsafe {
                RegEnumKeyExW(
                    key,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut length,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if status.is_err() {
                return false;
            }
            index += 1;
            let mut child = HKEY::default();
            let opened =
                unsafe { RegOpenKeyExW(key, PCWSTR(name.as_ptr()), Some(0), KEY_READ, &mut child) };
            if opened.is_ok() {
                let found = walk(child, depth - 1);
                unsafe {
                    let _ = RegCloseKey(child);
                }
                if found {
                    return true;
                }
            }
        }
    }
    let mut root = HKEY::default();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!(r"System\CurrentControlSet\Services\Dnscache\InterfaceSpecificParameters"),
            Some(0),
            KEY_READ,
            &mut root,
        )
    };
    if opened.is_err() {
        return false;
    }
    let found = walk(root, 4);
    unsafe {
        let _ = RegCloseKey(root);
    }
    found
}

// ----- processes -------------------------------------------------------------------------------------------------

fn processes() -> Result<Vec<(u32, String)>, PlatformError> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|error| failed("process list", error))?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut result = Vec::new();
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let length = entry
                    .szExeFile
                    .iter()
                    .position(|ch| *ch == 0)
                    .unwrap_or(entry.szExeFile.len());
                result.push((
                    entry.th32ProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..length]),
                ));
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
        Ok(result)
    }
}

fn kill_by_name(image_name: &str) -> Result<usize, PlatformError> {
    let mut killed = 0;
    for (pid, name) in processes()? {
        if !name.eq_ignore_ascii_case(image_name) || pid == std::process::id() {
            continue;
        }
        unsafe {
            if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                if TerminateProcess(handle, 1).is_ok() {
                    killed += 1;
                }
                let _ = CloseHandle(handle);
            }
        }
    }
    Ok(killed)
}

// ----- services --------------------------------------------------------------------------------------------------

fn list_services() -> Result<Vec<ServiceInfo>, PlatformError> {
    unsafe {
        let manager = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE)
            .map_err(|error| failed("service manager", error))?;
        let service_types = SERVICE_WIN32 | SERVICE_DRIVER;
        let mut needed = 0u32;
        let mut count = 0u32;
        let mut resume = 0u32;
        let _ = EnumServicesStatusExW(
            manager,
            SC_ENUM_PROCESS_INFO,
            service_types,
            SERVICE_STATE_ALL,
            None,
            &mut needed,
            &mut count,
            Some(&mut resume),
            PCWSTR::null(),
        );
        // u64 storage keeps the entries (which hold pointers) aligned.
        let mut storage = vec![0u64; needed as usize / 8 + 1];
        let buffer =
            std::slice::from_raw_parts_mut(storage.as_mut_ptr() as *mut u8, storage.len() * 8);
        resume = 0;
        let enumerated = EnumServicesStatusExW(
            manager,
            SC_ENUM_PROCESS_INFO,
            service_types,
            SERVICE_STATE_ALL,
            Some(buffer),
            &mut needed,
            &mut count,
            Some(&mut resume),
            PCWSTR::null(),
        );
        let result = enumerated.map(|()| {
            let entries = std::slice::from_raw_parts(
                storage.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW,
                count as usize,
            );
            entries
                .iter()
                .map(|entry| {
                    let status = entry.ServiceStatusProcess;
                    ServiceInfo {
                        name: entry.lpServiceName.to_string().unwrap_or_default(),
                        display_name: entry.lpDisplayName.to_string().unwrap_or_default(),
                        running: status.dwCurrentState == SERVICE_RUNNING,
                        stop_pending: status.dwCurrentState == SERVICE_STOP_PENDING,
                        is_driver: (status.dwServiceType.0 & SERVICE_DRIVER.0) != 0,
                    }
                })
                .collect()
        });
        let _ = CloseServiceHandle(manager);
        result.map_err(|error| failed("service list", error))
    }
}

fn delete_service(name: &str) -> Result<(), PlatformError> {
    unsafe {
        let manager = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|error| failed("service manager", error))?;
        let service = match OpenServiceW(
            manager,
            &HSTRING::from(name),
            SERVICE_STOP | SERVICE_QUERY_STATUS | ACCESS_DELETE,
        ) {
            Ok(service) => service,
            Err(error) => {
                let _ = CloseServiceHandle(manager);
                return if error.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() {
                    Ok(())
                } else {
                    Err(failed(&format!("служба {name}"), error))
                };
            }
        };
        let mut status = SERVICE_STATUS::default();
        let _ = ControlService(service, SERVICE_CONTROL_STOP, &mut status);
        for _ in 0..50 {
            if QueryServiceStatus(service, &mut status).is_err()
                || status.dwCurrentState == SERVICE_STOPPED
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let deleted = DeleteService(service);
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(manager);
        tracing::info!("service {name} removed: {deleted:?}");
        // "Marked for deletion" still counts: it disappears once released.
        deleted.or_else(|error| {
            if error.code().0 as u32 == 0x8007_0430 {
                Ok(())
            } else {
                Err(failed(&format!("служба {name}"), error))
            }
        })
    }
}

impl System for WindowsSystem {
    fn is_elevated(&self) -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut size = 0u32;
            let queried = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut elevation as *mut TOKEN_ELEVATION as *mut c_void),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            );
            let _ = CloseHandle(token);
            queried.is_ok() && elevation.TokenIsElevated != 0
        }
    }

    fn relaunch_elevated(&self) -> Result<(), PlatformError> {
        let exe = std::env::current_exe()?;
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("runas"),
                &HSTRING::from(exe.as_os_str()),
                w!("--relaunched"),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize > 32 {
            Ok(())
        } else {
            Err(PlatformError::Failed(
                "перезапуск отменён или не удался".into(),
            ))
        }
    }

    fn set_autostart(&self, enabled: bool) -> Result<(), PlatformError> {
        let schtasks = system_tool("schtasks.exe");
        if !enabled {
            let (exists, _) = run_tool(&schtasks, &["/Query", "/TN", TASK_NAME])?;
            if exists {
                let (deleted, _) = run_tool(&schtasks, &["/Delete", "/TN", TASK_NAME, "/F"])?;
                if !deleted {
                    return Err(PlatformError::Failed(
                        "не удалось удалить задачу автозапуска".into(),
                    ));
                }
            }
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        let user = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        );
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>zapret-gui: запуск при входе в систему</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>--autostart</Arguments></Exec></Actions>
</Task>
"#,
            user = xml_escape(&user),
            exe = xml_escape(&exe.display().to_string()),
        );
        // UTF-16LE with BOM, in the admin-only data dir so nobody can swap it before schtasks reads it.
        let path = self.data_dir.join("autostart-task.xml");
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        std::fs::write(&path, bytes)?;
        let path_text = path.display().to_string();
        let (created, output) = run_tool(
            &schtasks,
            &["/Create", "/TN", TASK_NAME, "/XML", &path_text, "/F"],
        )?;
        let _ = std::fs::remove_file(&path);
        if created {
            Ok(())
        } else {
            Err(PlatformError::Failed(format!(
                "не удалось создать задачу автозапуска: {output}"
            )))
        }
    }

    fn services(&self) -> Result<Vec<ServiceInfo>, PlatformError> {
        list_services()
    }

    fn remove_service(&self, name: &str) -> Result<(), PlatformError> {
        delete_service(name)
    }

    fn process_names(&self) -> Result<Vec<String>, PlatformError> {
        Ok(processes()?.into_iter().map(|(_, name)| name).collect())
    }

    fn kill_processes(&self, image_name: &str) -> Result<usize, PlatformError> {
        kill_by_name(image_name)
    }

    fn system_facts(&self) -> SystemFacts {
        const INTERNET_SETTINGS: &str =
            r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
        let proxy_enabled =
            reg_dword(HKEY_CURRENT_USER, INTERNET_SETTINGS, "ProxyEnable") == Some(1);
        let tcp_timestamps = run_tool(
            &system_tool("netsh.exe"),
            &["interface", "tcp", "show", "global"],
        )
        .ok()
        .and_then(|(_, output)| {
            output
                .lines()
                .map(str::to_lowercase)
                .find(|line| line.contains("timestamps"))
                .map(|line| line.contains("enabled"))
        });
        SystemFacts {
            proxy_enabled,
            proxy_server: if proxy_enabled {
                reg_string(HKEY_CURRENT_USER, INTERNET_SETTINGS, "ProxyServer").unwrap_or_default()
            } else {
                String::new()
            },
            doh_configured: doh_configured(),
            tcp_timestamps,
        }
    }

    fn enable_tcp_timestamps(&self) -> Result<(), PlatformError> {
        let (ok, output) = run_tool(
            &system_tool("netsh.exe"),
            &["interface", "tcp", "set", "global", "timestamps=enabled"],
        )?;
        if ok {
            Ok(())
        } else {
            Err(PlatformError::Failed(format!("netsh: {}", output.trim())))
        }
    }

    fn hosts_path(&self) -> PathBuf {
        system_root().join(r"System32\drivers\etc\hosts")
    }

    fn appdata_dir(&self) -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }

    fn secure_data_dir(&self, dir: &Path) -> Result<(), PlatformError> {
        // SIDs, not names: group names are localized. Administrators / SYSTEM full, Users read+execute.
        let icacls = system_tool("icacls.exe");
        let dir_text = dir.display().to_string();
        run_tool(
            &icacls,
            &[&dir_text, "/setowner", "*S-1-5-32-544", "/T", "/C", "/Q"],
        )?;
        let (ok, output) = run_tool(
            &icacls,
            &[
                &dir_text,
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-32-544:(OI)(CI)F",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-545:(OI)(CI)RX",
                "/Q",
            ],
        )?;
        if !ok {
            return Err(PlatformError::Failed(format!("icacls: {}", output.trim())));
        }
        // Children only inherit from the folder: drop any explicit entries planted before.
        let has_children = std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some());
        if has_children {
            let children = format!("{dir_text}\\*");
            run_tool(&icacls, &[&children, "/reset", "/T", "/C", "/Q"])?;
        }
        Ok(())
    }

    fn ping(&self, host: &str, timeout: Duration) -> Option<u32> {
        let address = (host, 0)
            .to_socket_addrs()
            .ok()?
            .find_map(|address| match address.ip() {
                IpAddr::V4(ip) => Some(ip),
                // shortcut: IPv4 only (like upstream's typical targets); add Icmp6SendEcho2 if IPv6-only hosts matter.
                IpAddr::V6(_) => None,
            })?;
        unsafe {
            let handle = IcmpCreateFile().ok()?;
            let request = [0x5Au8; 32];
            let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + request.len() + 8];
            let count = IcmpSendEcho(
                handle,
                u32::from_ne_bytes(address.octets()),
                request.as_ptr() as *const c_void,
                request.len() as u16,
                None,
                reply.as_mut_ptr() as *mut c_void,
                reply.len() as u32,
                timeout.as_millis() as u32,
            );
            let _ = IcmpCloseHandle(handle);
            if count == 0 {
                return None;
            }
            let echo = &*(reply.as_ptr() as *const ICMP_ECHO_REPLY);
            (echo.Status == 0).then_some(echo.RoundTripTime)
        }
    }

    fn wait_network_change(&self) {
        // Synchronous form (both pointers null): blocks until an IPv4 address changes.
        let status = unsafe { NotifyAddrChange(std::ptr::null_mut(), std::ptr::null()) };
        if status != 0 {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    fn user_env_var(&self, name: &str) -> Option<String> {
        reg_string(HKEY_CURRENT_USER, "Environment", name)
    }

    fn play_sound(&self, path: &Path) {
        let played = unsafe {
            PlaySoundW(
                &HSTRING::from(path),
                None,
                SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
            )
        };
        if !played.as_bool() {
            tracing::warn!("could not play {}", path.display());
        }
    }
}

pub struct WindowsDriver {
    system: Arc<dyn System>,
}

fn pump_lines(
    stream: impl AsyncRead + Unpin + Send + 'static,
    output: mpsc::UnboundedSender<String>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if output.send(line).is_err() {
                return;
            }
        }
    });
}

#[async_trait::async_trait]
impl WinwsDriver for WindowsDriver {
    async fn start(
        &self,
        spec: &WinwsLaunchSpec,
        output: mpsc::UnboundedSender<String>,
    ) -> Result<RunningWinws, PlatformError> {
        let mut child = tokio::process::Command::new(&spec.executable)
            .args(&spec.args)
            .current_dir(&spec.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        if let Some(stdout) = child.stdout.take() {
            pump_lines(stdout, output.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            pump_lines(stderr, output);
        }
        let (handle, control) = RunningWinws::new();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                _ = control.kill_requested => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let code = status.ok().and_then(|status| status.code()).unwrap_or(-1);
            let _ = control.exit.send(Some(code));
        });
        Ok(handle)
    }

    async fn kill_leftovers(&self) -> Result<usize, PlatformError> {
        let system = self.system.clone();
        tokio::task::spawn_blocking(move || system.kill_processes("winws.exe"))
            .await
            .map_err(|error| PlatformError::Failed(error.to_string()))?
    }
}
