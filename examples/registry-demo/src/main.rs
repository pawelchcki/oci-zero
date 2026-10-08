//! Minimal hosted HTTP transport; the store and router are both no_std.
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};

use oci_zero::server::serve;
use oci_zero_registry_demo::STORE;

fn handle(mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut input = [0u8; 8192];
    let mut len = 0;
    let header_length = loop {
        if len == input.len() {
            stream.write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            return Ok(());
        }
        let read = stream.read(&mut input[len..])?;
        if read == 0 {
            return Ok(());
        }
        len += read;
        if let Some(end) = input[..len]
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
        {
            break end + 4;
        }
    };
    let request =
        std::str::from_utf8(&input[..header_length]).map_err(|_| io::ErrorKind::InvalidData)?;
    let mut lines = request.split("\r\n");
    let mut start = lines.next().unwrap_or("").split_whitespace();
    let method = start.next().ok_or(io::ErrorKind::InvalidData)?;
    let target = start.next().ok_or(io::ErrorKind::InvalidData)?;
    let origin = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case("origin"))
        .map(|(_, value)| value.trim())
        .filter(|value| {
            !value.bytes().any(|b| b.is_ascii_control())
                && (value.starts_with("http://")
                    || value.starts_with("https://")
                    || *value == "null")
        });
    let mut scratch = [0; 4096];
    let result = serve(&STORE, method, target, &mut scratch)
        .map_err(|_| io::Error::other("registry listing buffer too small"))?;
    write!(stream, "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nDocker-Distribution-API-Version: registry/2.0\r\nVary: Origin\r\n", result.status, reason(result.status), result.media_type, result.content_length)?;
    if let Some(origin) = origin {
        write!(stream, "Access-Control-Allow-Origin: {origin}\r\nAccess-Control-Allow-Credentials: true\r\nAccess-Control-Allow-Methods: GET, HEAD, OPTIONS\r\nAccess-Control-Allow-Headers: Accept, Content-Type, Range\r\nAccess-Control-Expose-Headers: Docker-Content-Digest, Docker-Distribution-API-Version, Content-Length, Link\r\nAccess-Control-Max-Age: 86400\r\nAccess-Control-Allow-Private-Network: true\r\n")?;
    }
    if result.status == 405 {
        stream.write_all(b"Allow: GET, HEAD, OPTIONS\r\n")?;
    }
    if let Some(digest) = result.digest {
        write!(stream, "Docker-Content-Digest: {digest}\r\n")?;
    }
    if let Some(link) = result.next {
        write!(stream, "Link: {link}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.write_all(result.body)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    }
}

fn main() -> io::Result<()> {
    let address = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8787".into());
    let listener = TcpListener::bind(&address)?;
    eprintln!("In-memory OCI demo: http://{}", listener.local_addr()?);
    for stream in listener.incoming() {
        if let Err(error) = stream.and_then(handle) {
            eprintln!("request: {error}");
        }
    }
    Ok(())
}
