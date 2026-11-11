//! Looks for Macs to connect to: iPhones on a cable (asked of usbmuxd every second and a half) and Veyron
//! announcing itself on the local network (Bonjour, `_henge._tcp`, from builds with protocol 2).
use crate::session::{self, ServerInfo, Target};
use crate::usbmux;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Found {
    pub target: Target,
    /// The phone's name, or its address.
    pub title: String,
    pub usb: bool,
    /// Only from a Veyron with protocol 2; None says nothing either way.
    pub info: Option<ServerInfo>,
}

#[derive(Default)]
pub struct Discovery {
    pub usb: Mutex<Vec<Found>>,
    pub wifi: Mutex<Vec<Found>>,
    /// usbmuxd itself is missing: no phone on a cable can be seen.
    pub no_usbmuxd: AtomicBool,
    /// While a Mac is on screen nothing needs looking for.
    pub paused: AtomicBool,
}

impl Discovery {
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Arc<Discovery> {
        let d = Arc::new(Discovery::default());
        let wake = Arc::new(wake);
        let (usb, wake_usb) = (d.clone(), wake.clone());
        std::thread::spawn(move || usb.watch_usb(&*wake_usb));
        let wifi = d.clone();
        std::thread::spawn(move || wifi.watch_wifi(&*wake));
        d
    }

    pub fn all(&self) -> Vec<Found> {
        let mut all = self.usb.lock().unwrap().clone();
        all.extend(self.wifi.lock().unwrap().iter().cloned());
        all
    }

    fn watch_usb(&self, wake: &dyn Fn()) {
        let mut names: HashMap<String, String> = HashMap::new();
        let mut last = String::new();
        loop {
            if !self.paused.load(Ordering::Relaxed) {
                let devices = usbmux::devices();
                self.no_usbmuxd.store(devices.is_err(), Ordering::Relaxed);
                let mut found = Vec::new();
                for dev in devices.unwrap_or_default() {
                    if !names.contains_key(&dev.udid) {
                        if let Some(name) = usbmux::device_name(dev.id) {
                            names.insert(dev.udid.clone(), name);
                        }
                    }
                    let target = Target::Usb { udid: dev.udid.clone() };
                    let info = session::status(&target);
                    found.push(Found { title: names.get(&dev.udid).cloned().unwrap_or_else(|| "iPhone".into()), target, usb: true, info });
                }
                let now = format!("{found:?}{}", self.no_usbmuxd.load(Ordering::Relaxed));
                *self.usb.lock().unwrap() = found;
                if now != last {
                    last = now;
                    wake();
                }
            }
            std::thread::sleep(Duration::from_millis(1500));
        }
    }

    fn watch_wifi(&self, wake: &dyn Fn()) {
        let Ok(daemon) = mdns_sd::ServiceDaemon::new() else { return };
        let Ok(events) = daemon.browse("_henge._tcp.local.") else { return };
        let mut seen: HashMap<String, Found> = HashMap::new();
        while let Ok(event) = events.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(svc) => {
                    let Some(ip) = svc.get_addresses_v4().into_iter().next() else { continue };
                    let name = svc.txt_properties.get_property_val_str("name").unwrap_or("iPhone").to_string();
                    // The stream's port is in the text record; the announcement's own is the status port's.
                    let port = svc.txt_properties.get_property_val_str("port").and_then(|p| p.parse().ok()).unwrap_or(svc.get_port());
                    let target = Target::Net { host: ip.to_string(), port, code: String::new() };
                    // iOS gives apps "iPhone" for the device's name, so what tells phones apart is the Mac each runs.
                    let info = session::status(&target);
                    seen.insert(svc.get_fullname().to_string(), Found { target, title: name, usb: false, info });
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    seen.remove(&fullname);
                }
                _ => continue,
            }
            let mut list: Vec<Found> = seen.values().cloned().collect();
            list.sort_by(|a, b| a.title.cmp(&b.title));
            *self.wifi.lock().unwrap() = list;
            wake();
        }
    }
}
