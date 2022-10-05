//! Internal socket communication wrapper
use crate::reduce_timeout;
use crate::Error;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time;
use tokio_native_tls::TlsStream;

enum Stream {
    Plain(Box<TcpStream>),
    Tls(Box<TlsStream<TcpStream>>),
}

pub struct SStream {
    stream: Stream,
    timeout: Duration,
}

impl SStream {
    pub fn new(stream: TcpStream, timeout: Duration) -> Self {
        Self {
            stream: Stream::Plain(Box::new(stream)),
            timeout,
        }
    }
    pub fn new_tls(stream: TlsStream<TcpStream>, timeout: Duration) -> Self {
        Self {
            stream: Stream::Tls(Box::new(stream)),
            timeout,
        }
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    pub async fn write(&mut self, buf: &[u8]) -> Result<(), Error> {
        self.write_with_timeout(buf, self.timeout).await
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    pub async fn write_with_timeout(&mut self, buf: &[u8], timeout: Duration) -> Result<(), Error> {
        match self.stream {
            Stream::Plain(ref mut stream) => {
                time::timeout(timeout, stream.write_all(buf)).await??;
            }
            Stream::Tls(ref mut stream) => {
                time::timeout(timeout, stream.write_all(buf)).await??;
            }
        }
        Ok(())
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        self.read_with_timeout(buf, self.timeout).await
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    pub async fn read_with_timeout(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<(), Error> {
        match self.stream {
            Stream::Plain(ref mut stream) => {
                time::timeout(timeout, stream.read_exact(buf)).await??;
            }
            Stream::Tls(ref mut stream) => {
                time::timeout(timeout, stream.read_exact(buf)).await??;
            }
        }
        Ok(())
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    pub async fn read_frame(&mut self, max_length: Option<usize>) -> Result<Vec<u8>, Error> {
        self.read_frame_with_timeout(max_length, self.timeout).await
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    pub async fn read_frame_with_timeout(
        &mut self,
        max_length: Option<usize>,
        timeout: Duration,
    ) -> Result<Vec<u8>, Error> {
        let op_start = Instant::now();
        let mut len_buf: [u8; 4] = [0; 4];
        self.read_with_timeout(&mut len_buf, timeout).await?;
        let len = u32::from_le_bytes(len_buf);
        if let Some(max_len) = max_length {
            if len as usize > max_len {
                return Err(Error::invalid_data(format!("Frame too long ({})", len)));
            }
        }
        let mut buf = vec![0; len as usize];
        self.read_with_timeout(&mut buf, reduce_timeout(timeout, op_start))
            .await?;
        Ok(buf)
    }
    pub fn get_timeout(&self) -> Duration {
        self.timeout
    }
}
