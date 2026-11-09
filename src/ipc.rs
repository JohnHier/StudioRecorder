//! Opening files in an already running StudioRecorder, and the `.audioshare`
//! file association (spec §1.4: double-click = pair).

use crossbeam_channel::{Receiver, unbounded};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

const PORT: u16 = 47_811;

/// Hand a file to a running instance. True when it took it over.
pub fn handoff(path: &str) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], PORT));
    match TcpStream::connect_timeout(&addr, Duration::from_millis(300)) {
        Ok(mut s) => {
            let _ = s.write_all(format!("STUDIOREC {path}\n").as_bytes());
            true
        }
        Err(_) => false,
    }
}

/// Listen for files handed over by later instances.
pub fn listen() -> Option<Receiver<PathBuf>> {
    let l = TcpListener::bind(("127.0.0.1", PORT)).ok()?;
    let (tx, rx) = unbounded();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            if BufReader::new(s).read_line(&mut line).is_ok() {
                if let Some(p) = line.trim_end().strip_prefix("STUDIOREC ") {
                    let _ = tx.send(PathBuf::from(p));
                }
            }
        }
    });
    Some(rx)
}

/// Register `.audioshare` for the current user (no admin rights needed).
#[cfg(windows)]
pub fn register_file_type() {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    let Ok(exe) = std::env::current_exe() else { return };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let set = |key: &str, value: &str| {
        let k = wide(key);
        let v = wide(value);
        unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), std::ptr::null(), REG_SZ, v.as_ptr() as *const _, (v.len() * 2) as u32);
        }
    };
    set(r"Software\Classes\.audioshare", "StudioRecorder.AudioShareLink");
    set(r"Software\Classes\StudioRecorder.AudioShareLink", "AudioShare koppelbestand");
    set(r"Software\Classes\StudioRecorder.AudioShareLink\shell\open\command", &format!("\"{}\" \"%1\"", exe.display()));
    set(r"Software\Classes\StudioRecorder.AudioShareLink\DefaultIcon", &format!("\"{}\",0", exe.display()));
}

#[cfg(not(windows))]
pub fn register_file_type() {}
