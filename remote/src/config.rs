//! What the app remembers between runs: one JSON file in the user's configuration folder.
use crate::session::Tuning;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A Mac reached by address before, kept with its code so one click connects again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub host: String,
    pub port: u16,
    pub code: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub tuning: Tuning,
    pub hardware: bool,
    pub sound: bool,
    pub volume: f32,
    /// The pointer goes in and out of the Mac's picture freely, the Mac's following it. Off, or with a Henge
    /// too old for it: a click on the picture hands the mouse to the Mac until it is let go.
    pub seamless: bool,
    /// How far the Mac's pointer goes for the mouse's movement while the Mac holds it. Henge's own trackpad
    /// runs at about 2.
    pub pointer_speed: f32,
    /// Alt is Command and Super is Option, as a Mac keyboard has them laid out.
    pub alt_is_command: bool,
    pub controller: bool,
    pub clipboard: bool,
    /// Connect by itself to a phone on a cable as soon as it shares a Mac.
    pub auto_connect: bool,
    pub start_at_login: bool,
    pub fullscreen_on_connect: bool,
    pub show_stats: bool,
    pub saved: Vec<Saved>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tuning: Tuning::default(),
            hardware: true,
            sound: true,
            volume: 1.0,
            seamless: true,
            pointer_speed: 2.0,
            alt_is_command: false,
            controller: true,
            clipboard: true,
            auto_connect: false,
            start_at_login: false,
            fullscreen_on_connect: false,
            show_stats: false,
            saved: Vec::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("henge-remote").join("config.json"))
}

impl Config {
    pub fn load() -> Config {
        path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, text);
        }
    }

    pub fn remember(&mut self, host: &str, port: u16, code: &str) {
        self.saved.retain(|s| !(s.host == host && s.port == port));
        self.saved.insert(0, Saved { host: host.into(), port, code: code.into() });
        self.saved.truncate(8);
    }
}

/// Starts the app with the session, hidden until a Mac appears: an autostart entry on Linux, a Run key on
/// Windows, a launch agent on macOS.
pub fn set_start_at_login(on: bool) {
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.to_string_lossy();
    #[cfg(target_os = "linux")]
    {
        let Some(dir) = dirs::config_dir().map(|d| d.join("autostart")) else { return };
        let file = dir.join("henge-remote.desktop");
        if on {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(file, format!("[Desktop Entry]\nType=Application\nName=Henge Remote\nExec=\"{exe}\" --background\nIcon=henge-remote\nX-GNOME-Autostart-enabled=true\n"));
        } else {
            let _ = std::fs::remove_file(file);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let mut cmd = std::process::Command::new("reg");
        if on {
            cmd.args(["add", key, "/v", "HengeRemote", "/t", "REG_SZ", "/d", &format!("\"{exe}\" --background"), "/f"]);
        } else {
            cmd.args(["delete", key, "/v", "HengeRemote", "/f"]);
        }
        let _ = cmd.status();
    }
    #[cfg(target_os = "macos")]
    {
        let Some(dir) = dirs::home_dir().map(|d| d.join("Library/LaunchAgents")) else { return };
        let file = dir.join("app.henge.remote.plist");
        if on {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(
                file,
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>app.henge.remote</string><key>ProgramArguments</key><array><string>{exe}</string><string>--background</string></array><key>RunAtLoad</key><true/></dict></plist>\n"
                ),
            );
        } else {
            let _ = std::fs::remove_file(file);
        }
    }
}
