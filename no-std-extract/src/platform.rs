use core::convert::Infallible;
use core::future::Future;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use core::pin::pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use core::time::Duration;

use embedded_io::{Error, ErrorKind, ErrorType};
use embedded_io_async::{Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use rand_core::{TryCryptoRng, TryRng};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::fs::{Mode, OFlags, CWD};
use rustix::io::Errno;
use rustix::net::sockopt::Timeout;
use rustix::net::{AddressFamily, RecvFlags, SendFlags, SocketType};

#[derive(Clone, Copy, Debug)]
pub struct PlatformError;

impl core::fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("platform I/O error")
    }
}

impl core::error::Error for PlatformError {}

impl Error for PlatformError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

impl From<Errno> for PlatformError {
    fn from(_error: Errno) -> Self {
        Self
    }
}

pub struct FileReader(OwnedFd);

impl FileReader {
    pub fn open(path: &[u8]) -> Result<Self, PlatformError> {
        let fd = rustix::fs::openat(CWD, path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())?;
        Ok(Self(fd))
    }
}

impl ErrorType for FileReader {
    type Error = PlatformError;
}

impl Read for FileReader {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        read_fd(self.0.as_fd(), buffer)
    }
}

pub struct StdinReader(BorrowedFd<'static>);

impl StdinReader {
    pub fn new() -> Self {
        // SAFETY: The process inherits stdin and never closes or replaces it.
        Self(unsafe { rustix::stdio::stdin() })
    }
}

impl ErrorType for StdinReader {
    type Error = PlatformError;
}

impl Read for StdinReader {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        read_fd(self.0, buffer)
    }
}

pub struct TcpStack;

pub struct TcpStream(OwnedFd);

impl TcpConnect for TcpStack {
    type Error = PlatformError;
    type Connection<'a> = TcpStream;

    async fn connect<'a>(
        &'a self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        let family = match remote {
            SocketAddr::V4(_) => AddressFamily::INET,
            SocketAddr::V6(_) => AddressFamily::INET6,
        };
        let fd = rustix::net::socket(family, SocketType::STREAM, None)?;
        configure_socket(&fd)?;
        rustix::net::connect(&fd, &remote)?;
        Ok(TcpStream(fd))
    }
}

impl ErrorType for TcpStream {
    type Error = PlatformError;
}

impl Read for TcpStream {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        read_fd(self.0.as_fd(), buffer)
    }
}

impl Write for TcpStream {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        write_fd(self.0.as_fd(), buffer)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub struct DnsResolver {
    server: Ipv4Addr,
}

impl DnsResolver {
    pub const fn google() -> Self {
        Self {
            server: Ipv4Addr::new(8, 8, 8, 8),
        }
    }
}

impl Dns for DnsResolver {
    type Error = PlatformError;

    async fn get_host_by_name(
        &self,
        host: &str,
        address_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        if let Ok(address) = host.parse::<IpAddr>() {
            return Ok(address);
        }

        let query_type = match address_type {
            AddrType::IPv6 => 28,
            AddrType::IPv4 | AddrType::Either => 1,
        };
        query_dns(self.server, host.as_bytes(), query_type)
    }

    async fn get_host_by_address(
        &self,
        _address: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(PlatformError)
    }
}

pub struct OsRng(OwnedFd);

impl OsRng {
    pub fn open() -> Result<Self, PlatformError> {
        Ok(Self(rustix::fs::openat(
            CWD,
            b"/dev/urandom".as_slice(),
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    fn fill(&mut self, destination: &mut [u8]) {
        let mut filled = 0;
        while filled < destination.len() {
            match read_fd(self.0.as_fd(), &mut destination[filled..]) {
                Ok(0) | Err(_) => terminate(102),
                Ok(length) => filled += length,
            }
        }
    }
}

impl TryRng for OsRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0u8; 4];
        self.fill(&mut bytes);
        Ok(u32::from_ne_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0u8; 8];
        self.fill(&mut bytes);
        Ok(u64::from_ne_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Self::Error> {
        self.fill(destination);
        Ok(())
    }
}

impl TryCryptoRng for OsRng {}

pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = unsafe { Waker::from_raw(noop_raw_waker()) };
    let mut context = Context::from_waker(&waker);

    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
}

fn read_fd(fd: BorrowedFd<'_>, buffer: &mut [u8]) -> Result<usize, PlatformError> {
    loop {
        match rustix::io::read(fd, &mut *buffer) {
            Ok(length) => return Ok(length),
            Err(Errno::INTR) => {}
            Err(_) => return Err(PlatformError),
        }
    }
}

fn write_fd(fd: BorrowedFd<'_>, buffer: &[u8]) -> Result<usize, PlatformError> {
    loop {
        match rustix::io::write(fd, buffer) {
            Ok(length) => return Ok(length),
            Err(Errno::INTR) => {}
            Err(_) => return Err(PlatformError),
        }
    }
}

fn configure_socket(fd: &OwnedFd) -> Result<(), PlatformError> {
    let timeout = Some(Duration::from_secs(30));
    rustix::net::sockopt::set_socket_timeout(fd, Timeout::Recv, timeout)?;
    rustix::net::sockopt::set_socket_timeout(fd, Timeout::Send, timeout)?;

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    rustix::net::sockopt::set_socket_nosigpipe(fd, true)?;

    Ok(())
}

fn query_dns(server: Ipv4Addr, host: &[u8], query_type: u16) -> Result<IpAddr, PlatformError> {
    const TRANSACTION_ID: u16 = 0x4f43;

    let mut packet = [0u8; 512];
    packet[..12].copy_from_slice(&[
        (TRANSACTION_ID >> 8) as u8,
        TRANSACTION_ID as u8,
        0x01,
        0x00,
        0x00,
        0x01,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
    ]);
    let mut length = 12;
    for label in host.split(|byte| *byte == b'.') {
        if label.is_empty() || label.len() > 63 || length + 1 + label.len() + 5 > packet.len() {
            return Err(PlatformError);
        }
        packet[length] = label.len() as u8;
        length += 1;
        packet[length..length + label.len()].copy_from_slice(label);
        length += label.len();
    }
    packet[length] = 0;
    length += 1;
    packet[length..length + 4].copy_from_slice(&[(query_type >> 8) as u8, query_type as u8, 0, 1]);
    length += 4;

    let fd = rustix::net::socket(AddressFamily::INET, SocketType::DGRAM, None)?;
    configure_socket(&fd)?;
    rustix::net::connect(&fd, &SocketAddr::from((server, 53)))?;
    if rustix::net::send(&fd, &packet[..length], SendFlags::empty())? != length {
        return Err(PlatformError);
    }
    let (received, _) = rustix::net::recv(&fd, &mut packet, RecvFlags::empty())?;
    parse_dns_response(&packet[..received], TRANSACTION_ID, query_type)
}

fn parse_dns_response(
    packet: &[u8],
    transaction_id: u16,
    query_type: u16,
) -> Result<IpAddr, PlatformError> {
    if packet.len() < 12
        || read_u16(packet, 0)? != transaction_id
        || packet[2] & 0x80 == 0
        || packet[3] & 0x0f != 0
    {
        return Err(PlatformError);
    }

    let questions = read_u16(packet, 4)? as usize;
    let answers = read_u16(packet, 6)? as usize;
    let mut position = 12;
    for _ in 0..questions {
        position = skip_dns_name(packet, position)?;
        position = position.checked_add(4).ok_or(PlatformError)?;
        if position > packet.len() {
            return Err(PlatformError);
        }
    }

    for _ in 0..answers {
        position = skip_dns_name(packet, position)?;
        if position + 10 > packet.len() {
            return Err(PlatformError);
        }
        let record_type = read_u16(packet, position)?;
        let class = read_u16(packet, position + 2)?;
        let data_length = read_u16(packet, position + 8)? as usize;
        position += 10;
        let data = packet
            .get(position..position + data_length)
            .ok_or(PlatformError)?;

        if class == 1 && record_type == query_type {
            if record_type == 1 && data.len() == 4 {
                return Ok(IpAddr::V4(Ipv4Addr::new(
                    data[0], data[1], data[2], data[3],
                )));
            }
            if record_type == 28 && data.len() == 16 {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(data);
                return Ok(IpAddr::V6(Ipv6Addr::from(octets)));
            }
        }
        position += data_length;
    }

    Err(PlatformError)
}

fn skip_dns_name(packet: &[u8], mut position: usize) -> Result<usize, PlatformError> {
    loop {
        let length = *packet.get(position).ok_or(PlatformError)?;
        if length & 0xc0 == 0xc0 {
            return position
                .checked_add(2)
                .filter(|end| *end <= packet.len())
                .ok_or(PlatformError);
        }
        position += 1;
        if length == 0 {
            return Ok(position);
        }
        if length > 63 {
            return Err(PlatformError);
        }
        position = position
            .checked_add(length as usize)
            .filter(|end| *end <= packet.len())
            .ok_or(PlatformError)?;
    }
}

fn read_u16(bytes: &[u8], position: usize) -> Result<u16, PlatformError> {
    let bytes = bytes.get(position..position + 2).ok_or(PlatformError)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

const fn noop_raw_waker() -> RawWaker {
    const VTABLE: RawWakerVTable =
        RawWakerVTable::new(|_| noop_raw_waker(), |_| {}, |_| {}, |_| {});
    RawWaker::new(core::ptr::null(), &VTABLE)
}

fn terminate(status: i32) -> ! {
    extern "C" {
        fn _exit(status: i32) -> !;
    }

    unsafe { _exit(status) }
}
