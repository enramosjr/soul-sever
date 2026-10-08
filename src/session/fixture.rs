//! Localhost TCP peer for session tests. This is not a Soulseek server.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};

use tokio::net::{TcpListener, TcpStream};

pub struct Fixture {
    listener: TcpListener,
}

impl Fixture {
    pub async fn bind() -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        Ok(Self { listener })
    }

    pub fn addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub async fn accept(&self) -> io::Result<TcpStream> {
        let (stream, _) = self.listener.accept().await?;
        Ok(stream)
    }
}
