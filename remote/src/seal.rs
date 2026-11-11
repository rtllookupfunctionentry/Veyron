//! The connection to a phone over a network: the Noise handshake Noise_XX_25519_ChaChaPoly_SHA256 (done here
//! by the `snow` crate), then Noise's transport messages both ways, each behind its length. Each side's lasting
//! key tells the other who it is; a pairing settles, once, that the keys are the right ones. How it goes over
//! the wire is written out in ios/Henge/RemoteSeal.swift, the phone's half. A phone on a cable needs none of it.
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The phone's answer once it knows who is calling: come in, or pair first.
pub const GO: u8 = 0xA1;
pub const PAIR: u8 = 0xA2;
const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
/// Sent before the handshake and mixed into it as its prologue: what is being spoken, and which version.
const MAGIC: &[u8] = b"VYN2";
/// What goes into one transport message; Noise allows 65 535 bytes with the tag.
const CHUNK: usize = 60_000;

fn failed(e: snow::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("the phone and this computer could not agree on keys ({e})"))
}

/// What has been opened and not yet read, and how many messages have come.
#[derive(Default)]
struct Opened {
    count: u64,
    plain: Vec<u8>,
    at: usize,
}

/// A sealed connection. Copies share the keys and the two counters, so one thread may read while others write.
#[derive(Clone)]
pub struct Sealed {
    stream: Arc<TcpStream>,
    noise: Arc<snow::StatelessTransportState>,
    sent: Arc<Mutex<u64>>,
    opened: Arc<Mutex<Opened>>,
}

impl Sealed {
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    pub fn set_read_timeout(&self, t: Option<Duration>) {
        let _ = self.stream.set_read_timeout(t);
    }
}

impl Write for Sealed {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut sent = self.sent.lock().unwrap();
        let mut out = Vec::with_capacity(buf.len() + 18 * (buf.len() / CHUNK + 1));
        let mut sealed = [0u8; CHUNK + 16];
        for piece in buf.chunks(CHUNK) {
            let n = self.noise.write_message(*sent, piece, &mut sealed).map_err(failed)?;
            *sent += 1;
            out.extend_from_slice(&(n as u16).to_be_bytes());
            out.extend_from_slice(&sealed[..n]);
        }
        (&*self.stream).write_all(&out)?; // under the lock: messages must leave in the order they were counted
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for Sealed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut o = self.opened.lock().unwrap();
        while o.at == o.plain.len() {
            let mut len = [0u8; 2];
            (&*self.stream).read_exact(&mut len)?;
            let mut sealed = vec![0u8; u16::from_be_bytes(len) as usize];
            (&*self.stream).read_exact(&mut sealed)?;
            let mut plain = vec![0u8; sealed.len()];
            let n = self.noise.read_message(o.count, &sealed, &mut plain).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "what arrived was not sealed by the phone"))?;
            plain.truncate(n);
            (o.count, o.plain, o.at) = (o.count + 1, plain, 0);
        }
        let n = buf.len().min(o.plain.len() - o.at);
        buf[..n].copy_from_slice(&o.plain[o.at..o.at + n]);
        o.at += n;
        Ok(n)
    }
}

pub fn random() -> io::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(format!("no random numbers from the system ({e})")))?;
    Ok(b)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// This computer's lasting private key: made the first time and kept in the settings folder, for its user only.
pub fn identity() -> io::Result<[u8; 32]> {
    let dir = dirs::config_dir().ok_or_else(|| io::Error::other("no settings folder"))?.join("veyron-remote");
    let file = dir.join("identity");
    if let Ok(bytes) = std::fs::read(&file) {
        if let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice()) {
            return Ok(key);
        }
    }
    let key = random()?;
    std::fs::create_dir_all(&dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&file)?.write_all(&key)?;
    Ok(key)
}

/// What `connect` leaves: the sealed connection, the phone's lasting public key, and the handshake's hash,
/// which no two connections share and a pairing's number is made from.
pub struct Link {
    pub conn: Sealed,
    pub phone: Vec<u8>,
    pub binding: Vec<u8>,
}

/// The computer's part of the handshake (it begins), with `identity` as its lasting key.
pub fn connect(mut stream: TcpStream, identity: [u8; 32]) -> io::Result<Link> {
    let builder = snow::Builder::new(PATTERN.parse().map_err(failed)?);
    let mut noise = builder.local_private_key(&identity).map_err(failed)?.prologue(MAGIC).map_err(failed)?.build_initiator().map_err(failed)?;
    let (mut message, mut nothing) = ([0u8; 128], [0u8; 16]);
    let n = noise.write_message(&[], &mut message).map_err(failed)?; // -> e
    stream.write_all(&[MAGIC, &message[..n]].concat())?;
    let mut second = [0u8; 96]; // <- e, ee, s, es
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.read_exact(&mut second)?;
    stream.set_read_timeout(None)?;
    noise.read_message(&second, &mut nothing).map_err(failed)?;
    let n = noise.write_message(&[], &mut message).map_err(failed)?; // -> s, se
    stream.write_all(&message[..n])?;
    let binding = noise.get_handshake_hash().to_vec();
    let phone = noise.get_remote_static().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the phone showed no key"))?.to_vec();
    let noise = Arc::new(noise.into_stateless_transport_mode().map_err(failed)?);
    Ok(Link { conn: Sealed { stream: Arc::new(stream), noise, sent: Arc::default(), opened: Arc::default() }, phone, binding })
}

/// The first thing said on a sealed connection: whether this computer already knows the phone, its name, and
/// a promise (the hash) of the random bytes it will show if the phone asks to pair.
pub fn greeting(knows: bool, name: &str, pledge: &[u8; 32]) -> Vec<u8> {
    let name = &name.as_bytes()[..name.len().min(63)];
    [&[knows as u8, name.len() as u8], name, &Sha256::digest(pledge)[..]].concat()
}

/// The six digits both screens show when pairing: from the handshake's hash and a random value from each
/// side. The computer promised its value before seeing the phone's, and the phone gave its own before seeing
/// the computer's, so neither (nor anyone sitting between them) could steer the number.
pub fn number(binding: &[u8], computer: &[u8; 32], phone: &[u8; 32]) -> String {
    let hash = Sha256::digest([b"veyron pairing".as_slice(), binding, computer, phone].concat());
    format!("{:06}", u32::from_be_bytes(hash[..4].try_into().unwrap()) % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// The phone's side of the handshake, by the same library, to try this file's own part against.
    /// (tests/fake-phone.py has it written out by hand, as the phone's Swift does.)
    fn phone(mut stream: TcpStream, identity: [u8; 32]) -> Link {
        let mut first = [0u8; 36];
        stream.read_exact(&mut first).unwrap();
        assert_eq!(&first[..4], MAGIC);
        let mut noise = snow::Builder::new(PATTERN.parse().unwrap()).local_private_key(&identity).unwrap().prologue(MAGIC).unwrap().build_responder().unwrap();
        let (mut message, mut nothing) = ([0u8; 128], [0u8; 16]);
        noise.read_message(&first[4..], &mut nothing).unwrap();
        let n = noise.write_message(&[], &mut message).unwrap();
        assert_eq!(n, 96);
        stream.write_all(&message[..n]).unwrap();
        let mut third = [0u8; 64];
        stream.read_exact(&mut third).unwrap();
        noise.read_message(&third, &mut nothing).unwrap();
        let (binding, phone) = (noise.get_handshake_hash().to_vec(), noise.get_remote_static().unwrap().to_vec());
        let noise = Arc::new(noise.into_stateless_transport_mode().unwrap());
        Link { conn: Sealed { stream: Arc::new(stream), noise, sent: Arc::default(), opened: Arc::default() }, phone, binding }
    }

    #[test]
    fn both_ends_agree() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (pc_key, phone_key) = ([7u8; 32], [9u8; 32]);
        let server = std::thread::spawn(move || {
            let mut link = phone(listener.accept().unwrap().0, phone_key);
            let mut hello = [0u8; 38];
            link.conn.read_exact(&mut hello).unwrap();
            assert_eq!(&hello[..6], &[1, 4, b'd', b'e', b's', b'k']);
            assert_eq!(&hello[6..], Sha256::digest([3u8; 32]).as_slice());
            link.conn.write_all(&[GO]).unwrap();
            let big: Vec<u8> = (0..150_000u32).map(|i| i as u8).collect(); // three messages
            link.conn.write_all(&big).unwrap();
            (link.phone, link.binding)
        });
        let mut link = connect(TcpStream::connect(("127.0.0.1", port)).unwrap(), pc_key).unwrap();
        link.conn.write_all(&greeting(true, "desk", &[3u8; 32])).unwrap();
        let mut gate = [0u8; 1];
        link.conn.read_exact(&mut gate).unwrap();
        assert_eq!(gate[0], GO);
        let mut big = vec![0u8; 150_000];
        link.conn.read_exact(&mut big).unwrap();
        assert!(big.iter().enumerate().all(|(i, b)| *b == i as u8));
        let (computer, binding) = server.join().unwrap();
        assert_eq!(binding, link.binding, "both ends hold the same handshake hash");
        assert_eq!(computer.len(), 32);
        assert_ne!(computer, link.phone, "each end saw the other's lasting key, not its own");
        assert_eq!(number(&binding, &[1; 32], &[2; 32]), number(&link.binding, &[1; 32], &[2; 32]));
        assert_ne!(number(&binding, &[1; 32], &[2; 32]), number(&binding, &[1; 32], &[3; 32]));
    }
}
