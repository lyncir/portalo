use bevy::prelude::*;
use std::io::{Read, Write};
use std::net::TcpStream;

#[derive(Component)]
pub struct NetworkConnection {
    pub stream: Option<TcpStream>,
    pub remote_addr: String,
}

pub fn connect_to_service(addr: &str, port: u16) -> Result<TcpStream, Box<dyn std::error::Error>> {
    let stream = TcpStream::connect(format!("{}:{}", addr, port))?;
    stream.set_nonblocking(true)?;
    Ok(stream)
}

pub fn send_message(stream: &mut TcpStream, message: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    stream.write_all(message)?;
    stream.flush()?;
    Ok(())
}

pub fn receive_message(stream: &mut TcpStream) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut buffer = vec![0; 1024];
    match stream.read(&mut buffer) {
        Ok(n) => {
            buffer.truncate(n);
            Ok(buffer)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            Ok(Vec::new())
        }
        Err(e) => Err(Box::new(e)),
    }
}
