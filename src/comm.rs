//! Internal socket communication wrapper
use crate::Error;
use crate::reduce_timeout;
use async_trait::async_trait;
use std::marker::Unpin;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time;

//enum Stream {
//Plain(Box<TcpStream>),
//Tls(Box<TlsStream<TcpStream>>),
//}

pub struct SStream<S>
where
    S: AsyncReadExt + AsyncWriteExt + Send + Unpin,
{
    stream: S,
    timeout: Duration,
}

impl<S> SStream<S>
where
    S: AsyncReadExt + AsyncWriteExt + Send + Unpin,
{
    pub fn new(stream: S, timeout: Duration) -> Self {
        Self { stream, timeout }
    }
}

#[async_trait]
pub trait StreamHandler: Send {
    async fn write(&mut self, buf: &[u8]) -> Result<(), Error>;
    async fn write_with_timeout(&mut self, buf: &[u8], timeout: Duration) -> Result<(), Error>;
    async fn read(&mut self, buf: &mut [u8]) -> Result<(), Error>;
    async fn read_with_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> Result<(), Error>;
    async fn read_frame(&mut self, max_length: Option<usize>) -> Result<Vec<u8>, Error>;
    async fn read_frame_with_timeout(
        &mut self,
        max_length: Option<usize>,
        timeout: Duration,
    ) -> Result<Vec<u8>, Error>;
    fn get_timeout(&self) -> Duration;
}

#[async_trait]
impl<S> StreamHandler for SStream<S>
where
    S: AsyncReadExt + AsyncWriteExt + Send + Sync + Unpin,
{
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    async fn write(&mut self, buf: &[u8]) -> Result<(), Error> {
        self.write_with_timeout(buf, self.timeout).await
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    async fn write_with_timeout(&mut self, buf: &[u8], timeout: Duration) -> Result<(), Error> {
        time::timeout(timeout, self.stream.write_all(buf)).await??;
        Ok(())
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    async fn read(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        self.read_with_timeout(buf, self.timeout).await
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    async fn read_with_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> Result<(), Error> {
        time::timeout(timeout, self.stream.read_exact(buf)).await??;
        Ok(())
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[inline]
    async fn read_frame(&mut self, max_length: Option<usize>) -> Result<Vec<u8>, Error> {
        self.read_frame_with_timeout(max_length, self.timeout).await
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    async fn read_frame_with_timeout(
        &mut self,
        max_length: Option<usize>,
        timeout: Duration,
    ) -> Result<Vec<u8>, Error> {
        let op_start = Instant::now();
        let mut len_buf: [u8; 4] = [0; 4];
        self.read_with_timeout(&mut len_buf, timeout).await?;
        let len = u32::from_le_bytes(len_buf);
        if let Some(max_len) = max_length
            && len as usize > max_len
        {
            return Err(Error::invalid_data(format!("Frame too long ({})", len)));
        }
        let mut buf = vec![0; len as usize];
        self.read_with_timeout(&mut buf, reduce_timeout(timeout, op_start))
            .await?;
        Ok(buf)
    }
    fn get_timeout(&self) -> Duration {
        self.timeout
    }
}
