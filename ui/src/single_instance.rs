//! Single instance (D16): a local socket. A second launch asks the first to show its window, then exits.

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use interprocess::local_socket::{
    GenericNamespaced, Listener, ListenerOptions, Stream, prelude::*,
};

const SOCKET_NAME: &str = "zapret-gui.instance";

/// `Some(listener)`: we are the primary instance. `None`: another instance was asked to show itself.
/// `wait_for_previous`: after "restart as admin" the old instance may still hold the name for a moment.
pub fn acquire(wait_for_previous: bool) -> std::io::Result<Option<Listener>> {
    let deadline = Instant::now() + Duration::from_secs(if wait_for_previous { 10 } else { 0 });
    loop {
        let name = SOCKET_NAME.to_ns_name::<GenericNamespaced>()?;
        match ListenerOptions::new().name(name).create_sync() {
            Ok(listener) => return Ok(Some(listener)),
            Err(error) if Instant::now() < deadline => {
                tracing::info!("instance socket busy ({error}), waiting");
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(error) => {
                let name = SOCKET_NAME.to_ns_name::<GenericNamespaced>()?;
                return match Stream::connect(name) {
                    Ok(mut stream) => {
                        stream.write_all(b"show\n")?;
                        Ok(None)
                    }
                    Err(connect_error) => {
                        // Can't reach the other instance either: run without the guard rather than not at all.
                        tracing::warn!("single instance unavailable: {error} / {connect_error}");
                        Ok(Some(fallback_listener()?))
                    }
                };
            }
        }
    }
}

fn fallback_listener() -> std::io::Result<Listener> {
    let name = format!("{SOCKET_NAME}.{}", std::process::id()).to_ns_name::<GenericNamespaced>()?;
    ListenerOptions::new().name(name).create_sync()
}

/// Calls `on_show` (from the listener thread) whenever another launch asks us to show the window.
pub fn listen(listener: Listener, on_show: impl Fn() + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || {
            for connection in listener.incoming().filter_map(Result::ok) {
                let mut line = String::new();
                if BufReader::new(connection).read_line(&mut line).is_ok() && line.trim() == "show"
                {
                    on_show();
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!("single instance listener failed: {error}");
    }
}
