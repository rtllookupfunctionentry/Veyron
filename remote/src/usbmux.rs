//! Reaching an iPhone over its cable, through usbmuxd: Apple's own service on macOS and Windows (it comes
//! with Finder, iTunes or the Apple Devices app), libimobiledevice's on Linux. No iproxy in between.
use plist::{Dictionary, Value};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

/// One connection, whichever way it was made: a TCP socket, or the usbmuxd socket once it carries a tunnel.
pub enum Conn {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Conn {
    pub fn try_clone(&self) -> io::Result<Conn> {
        Ok(match self {
            Conn::Tcp(s) => Conn::Tcp(s.try_clone()?),
            #[cfg(unix)]
            Conn::Unix(s) => Conn::Unix(s.try_clone()?),
        })
    }

    pub fn shutdown(&self) {
        let _ = match self {
            Conn::Tcp(s) => s.shutdown(Shutdown::Both),
            #[cfg(unix)]
            Conn::Unix(s) => s.shutdown(Shutdown::Both),
        };
    }

    pub fn set_read_timeout(&self, t: Option<Duration>) {
        let _ = match self {
            Conn::Tcp(s) => s.set_read_timeout(t),
            #[cfg(unix)]
            Conn::Unix(s) => s.set_read_timeout(t),
        };
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Conn::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Conn::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsbDevice {
    pub id: u64,
    pub udid: String,
}

fn service() -> io::Result<Conn> {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect("/var/run/usbmuxd").map(Conn::Unix)
    }
    #[cfg(not(unix))]
    {
        TcpStream::connect(("127.0.0.1", 27015)).map(Conn::Tcp)
    }
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// One request and its answer: a 16-byte header [length, version 1, type 8 (plist), tag], little-endian, then XML.
fn call(c: &mut Conn, kind: &str, more: &[(&str, Value)]) -> io::Result<Dictionary> {
    let mut d = Dictionary::new();
    d.insert("MessageType".into(), kind.into());
    d.insert("ClientVersionString".into(), "veyron-remote".into());
    d.insert("ProgName".into(), "veyron-remote".into());
    d.insert("kLibUSBMuxVersion".into(), Value::Integer(3.into()));
    for (k, v) in more {
        d.insert((*k).into(), v.clone());
    }
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &Value::Dictionary(d)).map_err(|e| bad(&e.to_string()))?;
    let mut msg = Vec::with_capacity(16 + body.len());
    for field in [16 + body.len() as u32, 1, 8, 1] {
        msg.extend_from_slice(&field.to_le_bytes());
    }
    msg.extend_from_slice(&body);
    c.write_all(&msg)?;
    let mut head = [0u8; 16];
    c.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if !(16..=1 << 20).contains(&len) {
        return Err(bad("usbmuxd answered with an impossible length"));
    }
    let mut body = vec![0u8; len - 16];
    c.read_exact(&mut body)?;
    plist::from_bytes::<Value>(&body).ok().and_then(Value::into_dictionary).ok_or_else(|| bad("usbmuxd answered with something that is not a dictionary"))
}

/// The iPhones and iPads on a cable right now. An error means usbmuxd itself is not there.
pub fn devices() -> io::Result<Vec<UsbDevice>> {
    let mut c = service()?;
    c.set_read_timeout(Some(Duration::from_secs(3)));
    let reply = call(&mut c, "ListDevices", &[])?;
    let mut out = Vec::new();
    for dev in reply.get("DeviceList").and_then(Value::as_array).into_iter().flatten() {
        let Some(p) = dev.as_dictionary().and_then(|d| d.get("Properties")).and_then(Value::as_dictionary) else { continue };
        if p.get("ConnectionType").and_then(Value::as_string) != Some("USB") {
            continue; // usbmuxd also lists phones it knows over Wi-Fi sync; those are reached by address instead
        }
        if let (Some(id), Some(udid)) = (p.get("DeviceID").and_then(Value::as_unsigned_integer), p.get("SerialNumber").and_then(Value::as_string)) {
            out.push(UsbDevice { id, udid: udid.to_string() });
        }
    }
    Ok(out)
}

/// A tunnel to a TCP port on the phone. `ConnectionRefused` when nothing listens there (Veyron is not streaming).
pub fn connect(device: u64, port: u16) -> io::Result<Conn> {
    let mut c = service()?;
    c.set_read_timeout(Some(Duration::from_secs(5)));
    let reply = call(&mut c, "Connect", &[("DeviceID", Value::Integer(device.into())), ("PortNumber", Value::Integer((port.swap_bytes() as u64).into()))])?;
    c.set_read_timeout(None);
    match reply.get("Number").and_then(Value::as_unsigned_integer) {
        Some(0) => Ok(c),
        Some(3) => Err(io::Error::new(io::ErrorKind::ConnectionRefused, "nothing is listening on the phone")),
        Some(2) => Err(io::Error::new(io::ErrorKind::NotFound, "the phone is no longer plugged in")),
        other => Err(bad(&format!("usbmuxd refused the connection ({other:?})"))),
    }
}

/// The name the phone was given in Settings ("Sam's iPhone"), asked of its lockdown service, which answers
/// this one question without the computer being paired. None when it does not.
pub fn device_name(device: u64) -> Option<String> {
    let mut c = connect(device, 62078).ok()?;
    c.set_read_timeout(Some(Duration::from_secs(2)));
    let mut d = Dictionary::new();
    d.insert("Label".into(), "veyron-remote".into());
    d.insert("Request".into(), "GetValue".into());
    d.insert("Key".into(), "DeviceName".into());
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &Value::Dictionary(d)).ok()?;
    c.write_all(&(body.len() as u32).to_be_bytes()).ok()?;
    c.write_all(&body).ok()?;
    let mut len = [0u8; 4];
    c.read_exact(&mut len).ok()?;
    let len = u32::from_be_bytes(len) as usize;
    if len > 1 << 16 {
        return None;
    }
    let mut body = vec![0u8; len];
    c.read_exact(&mut body).ok()?;
    let reply = plist::from_bytes::<Value>(&body).ok()?.into_dictionary()?;
    reply.get("Value")?.as_string().map(str::to_string)
}
