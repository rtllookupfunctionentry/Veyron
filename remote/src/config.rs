//! What the app remembers between runs: one JSON file in the user's configuration folder.
use crate::session::Tuning;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A Mac reached by address before, kept so one click connects again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub host: String,
    pub port: u16,
}

/// A phone this computer has been paired with over the network: its lasting public key, in hex.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Paired {
    pub key: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub tuning: Tuning,
    pub hardware: bool,
    pub sound: bool,
    pub volume: f32,
    /// The pointer goes in and out of the Mac's picture freely, the Mac's following it. Off, or with a Veyron
    /// too old for it: a click on the picture hands the mouse to the Mac until it is let go.
    pub seamless: bool,
    /// How far the Mac's pointer goes for the mouse's movement while the Mac holds it. Veyron's own trackpad
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
    pub paired: Vec<Paired>,
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
            paired: Vec::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("veyron-remote").join("config.json"))
}

impl Config {
    pub fn load() -> Config {
        // Before the app was Veyron Remote its file was in a folder of the old name: still read, never written.
        let old = || dirs::config_dir().map(|d| d.join("henge-remote").join("config.json"));
        let bytes = path().and_then(|p| std::fs::read(p).ok()).or_else(|| std::fs::read(old()?).ok());
        bytes.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, text);
        }
    }

    pub fn remember(&mut self, host: &str, port: u16) {
        self.saved.retain(|s| !(s.host == host && s.port == port));
        self.saved.insert(0, Saved { host: host.into(), port });
        self.saved.truncate(8);
    }

    /// The user has compared the numbers: this phone's key is the one to expect from now on.
    pub fn pair(&mut self, key: &str, name: &str) {
        self.paired.retain(|p| p.key != key);
        self.paired.push(Paired { key: key.into(), name: name.into() });
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
        let file = dir.join("veyron-remote.desktop");
        if on {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(file, format!("[Desktop Entry]\nType=Application\nName=Veyron Remote\nExec=\"{exe}\" --background\nIcon=veyron-remote\nX-GNOME-Autostart-enabled=true\n"));
        } else {
            let _ = std::fs::remove_file(file);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let mut cmd = std::process::Command::new("reg");
        if on {
            cmd.args(["add", key, "/v", "VeyronRemote", "/t", "REG_SZ", "/d", &format!("\"{exe}\" --background"), "/f"]);
        } else {
            cmd.args(["delete", key, "/v", "VeyronRemote", "/f"]);
        }
        let _ = cmd.status();
    }
    #[cfg(target_os = "macos")]
    {
        let Some(dir) = dirs::home_dir().map(|d| d.join("Library/LaunchAgents")) else { return };
        let file = dir.join("app.veyron.remote.plist");
        if on {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(
                file,
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>app.veyron.remote</string><key>ProgramArguments</key><array><string>{exe}</string><string>--background</string></array><key>RunAtLoad</key><true/></dict></plist>\n"
                ),
            );
        } else {
            let _ = std::fs::remove_file(file);
        }
    }
}
