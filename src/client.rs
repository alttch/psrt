//! PSRT client module
use crate::comm::{SStream, StreamHandler};
use crate::is_unix_socket;
use crate::reduce_timeout;
use crate::Message;
use crate::COMM_INSECURE;
use crate::COMM_TLS;
use crate::OP_BYE;
use crate::OP_NOP;
use crate::OP_PUBLISH;
use crate::OP_PUBLISH_REPL;
use crate::OP_SUBSCRIBE;
use crate::OP_UNSUBSCRIBE;
use crate::RESPONSE_ERR_ACCESS;
use crate::RESPONSE_NOT_REQUIRED;
use crate::RESPONSE_OK;
use crate::RESPONSE_OK_WAITING;
use crate::{Error, ErrorKind};
use log::trace;
use serde::{Deserialize, Deserializer};
use std::path::Path;
use std::sync::atomic;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, ToSocketAddrs, UdpSocket, UnixStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time;
use tokio::time::sleep;
use tokio_native_tls::{native_tls, TlsConnector};

static ERR_COMMUNCATION_LOST: &str = "Communication lost";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_QUEUE_SIZE: usize = 8192;

#[derive(Eq, PartialEq, Debug)]
enum ControlCommand {
    Nop,
    Subscribe(String),
    SubscribeBulk(Vec<String>),
    Unsubscribe(String),
    UnsubscribeBulk(Vec<String>),
    Publish(u8, String, Vec<u8>),
    PublishRepl(u8, Arc<String>, Arc<Vec<u8>>, u64),
    Bye,
}

macro_rules! err_comm {
    () => {
        Err(Error::internal(ERR_COMMUNCATION_LOST))
    };
}

macro_rules! exec_command {
    ($channel: expr, $cmd: expr, $timeout: expr) => {{
        let (tx, rx) = oneshot::channel();
        if time::timeout(
            $timeout,
            $channel.send(Command {
                control_command: $cmd,
                response_channel: Some(tx),
            }),
        )
        .await?
        .is_err()
        {
            return err_comm!();
        }
        time::timeout($timeout, rx)
            .await?
            .unwrap_or_else(|_| err_comm!())
    }};
}

#[allow(clippy::cast_possible_truncation)]
impl ControlCommand {
    fn as_bytes(&self) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        match self {
            ControlCommand::Nop => buf.push(OP_NOP),
            ControlCommand::Subscribe(topic) => {
                let topic = topic.as_bytes();
                buf.push(OP_SUBSCRIBE);
                buf.extend((topic.len() as u32).to_le_bytes());
                buf.extend(topic);
            }
            ControlCommand::SubscribeBulk(topics) => {
                buf.push(OP_SUBSCRIBE);
                let mut len: u32 = 0;
                for t in topics {
                    len += t.as_bytes().len() as u32 + 1;
                }
                buf.extend(len.to_le_bytes());
                for t in topics {
                    buf.extend(t.as_bytes());
                    buf.push(0x0);
                }
            }
            ControlCommand::Unsubscribe(topic) => {
                let topic = topic.as_bytes();
                buf.push(OP_UNSUBSCRIBE);
                buf.extend((topic.len() as u32).to_le_bytes());
                buf.extend(topic);
            }
            ControlCommand::UnsubscribeBulk(topics) => {
                buf.push(OP_UNSUBSCRIBE);
                let mut len: u32 = 0;
                for t in topics {
                    len += t.as_bytes().len() as u32 + 1;
                }
                buf.extend(len.to_le_bytes());
                for t in topics {
                    buf.extend(t.as_bytes());
                    buf.push(0x0);
                }
            }
            ControlCommand::Publish(priority, topic, message) => {
                buf.push(OP_PUBLISH);
                buf.push(*priority);
                buf.extend(
                    (topic.as_bytes().len() as u32 + 1 + message.len() as u32).to_le_bytes(),
                );
                buf.extend(topic.as_bytes());
                buf.push(0x00);
            }
            ControlCommand::PublishRepl(_, topic, _, _) => {
                buf.push(OP_PUBLISH_REPL);
                buf.extend((topic.as_bytes().len() as u32).to_le_bytes());
                buf.extend(topic.as_bytes());
            }
            ControlCommand::Bye => {
                buf.push(OP_BYE);
            }
        }
        buf
    }
}

struct Command {
    control_command: ControlCommand,
    response_channel: Option<oneshot::Sender<Result<(), Error>>>,
}

fn get_default_timeout() -> Duration {
    DEFAULT_TIMEOUT
}

fn get_default_queue_size() -> usize {
    DEFAULT_QUEUE_SIZE
}

fn de_float_as_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Duration::from_secs_f64(f64::deserialize(deserializer)?))
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
/// Client configuration
///
/// "set_" and "disable_" functions have to be run in chain
///
/// "update_" functions alter the existing configuration
///
/// ```rust
/// use psrt::client::Config;
/// use std::time::Duration;
///
/// let config = Config::new("localhost:2873").
///     set_auth("test", "123").
///     set_timeout(Duration::from_secs(5)).
///     build();
///
/// assert_eq!(config.path(), "localhost:2873");
/// ```
pub struct Config {
    // make fields permanent to let store config in wrappers and pools
    path: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    password: String,
    #[serde(
        default = "get_default_timeout",
        deserialize_with = "de_float_as_duration"
    )]
    timeout: Duration,
    #[serde(
        default = "get_default_timeout",
        deserialize_with = "de_float_as_duration"
    )]
    socket_wait_timeout: Duration,
    #[serde(default = "get_default_queue_size")]
    queue_size: usize,
    #[serde(default)]
    tls: bool,
    tls_ca: Option<String>,
    #[serde(default)]
    need_data_stream: bool,
}

impl Config {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            user: String::new(),
            password: String::new(),
            timeout: get_default_timeout(),
            socket_wait_timeout: get_default_timeout(),
            queue_size: get_default_queue_size(),
            tls: false,
            tls_ca: None,
            need_data_stream: true,
        }
    }
    #[inline]
    pub fn set_auth(mut self, user: &str, password: &str) -> Self {
        user.clone_into(&mut self.user);
        password.clone_into(&mut self.password);
        self
    }
    /// Do not connect the data socket
    #[inline]
    pub fn disable_data_stream(mut self) -> Self {
        self.need_data_stream = false;
        self
    }
    #[inline]
    pub fn set_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Set socket wait timeout (for UNIX sockets only)
    #[inline]
    pub fn set_socket_wait_timeout(mut self, timeout: Duration) -> Self {
        self.socket_wait_timeout = timeout;
        self
    }
    /// Set queue size
    ///
    /// The default queue size is 8192 commands
    ///
    /// For true atomic operations set queue size to 1
    #[inline]
    pub fn set_queue_size(mut self, size: usize) -> Self {
        self.queue_size = size;
        self
    }
    #[inline]
    pub fn set_tls(mut self, tls: bool) -> Self {
        self.tls = tls;
        self
    }
    /// Set CA
    ///
    /// This is NOT a file path. CA certs MUST be pre-loaded in string
    #[inline]
    pub fn set_tls_ca(mut self, ca: Option<String>) -> Self {
        self.tls_ca = ca;
        self
    }
    #[inline]
    pub fn build(self) -> Self {
        self
    }
    /// Get path
    #[inline]
    pub fn path(&self) -> &str {
        &self.path
    }
    /// Get timeout
    #[inline]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
    #[inline]
    pub fn socket_wait_timeout(&self) -> Duration {
        self.socket_wait_timeout
    }
    /// Get TLS CA
    #[inline]
    pub fn tls_ca(&self) -> Option<&String> {
        self.tls_ca.as_ref()
    }
    /// Update TLS CA
    /// This is NOT a file path. CA certs should be pre-loaded in string
    #[inline]
    pub fn update_tls_ca(&mut self, value: String) {
        self.tls_ca = Some(value);
    }
    /// Update path
    #[inline]
    pub fn update_path(&mut self, path: &str) {
        path.clone_into(&mut self.path);
    }
}

/// PSRT Client
pub struct Client {
    control_fut: JoinHandle<()>,
    control_ping_fut: JoinHandle<()>,
    data_fut: Option<JoinHandle<()>>,
    control_channel: async_channel::Sender<Command>,
    data_channel: Option<async_channel::Receiver<Message>>,
    connected: Arc<atomic::AtomicBool>,
    timeout: Duration,
}

impl Client {
    /// # Errors
    ///
    /// Will return Err on communcation errors
    #[allow(clippy::too_many_lines)]
    pub async fn connect(config: &Config) -> Result<Self, Error> {
        trace!("config: {:?}", config);
        trace!("version: {}", crate::VERSION);
        let tls_builder: Option<native_tls::TlsConnectorBuilder> = if config.tls {
            let mut builder = native_tls::TlsConnector::builder();
            if let Some(ref ca) = config.tls_ca {
                builder.add_root_certificate(native_tls::Certificate::from_pem(ca.as_bytes())?);
            }
            Some(builder)
        } else {
            None
        };
        let is_unix = is_unix_socket(&config.path);
        // Connect control stream
        trace!("connecting control stream to {}", config.path);
        let mut op_start = Instant::now();
        let timeout = config.timeout;
        let path_domain = if is_unix {
            None
        } else {
            Some(
                config
                    .path
                    .rsplitn(2, ':')
                    .last()
                    .ok_or_else(|| Error::invalid_data("Invalid domain"))?,
            )
        };
        let mut control_stream: Box<dyn StreamHandler> = {
            let mut greeting = Vec::new();
            greeting.extend(crate::CONTROL_HEADER);
            if is_unix {
                let path = Path::new(&config.path);
                let sleep_step = Duration::from_millis(100);
                if !path.exists() {
                    while !path.exists() {
                        tokio::time::sleep(sleep_step).await;
                        if op_start.elapsed() > config.socket_wait_timeout {
                            return Err(Error::io("UNIX socket not found"));
                        }
                    }
                    op_start = Instant::now();
                }
                let mut c_control_stream =
                    time::timeout(timeout, UnixStream::connect(&config.path)).await??;
                greeting.push(COMM_INSECURE);
                time::timeout(
                    reduce_timeout(timeout, op_start),
                    c_control_stream.write_all(&greeting),
                )
                .await??;
                Box::new(SStream::new(c_control_stream, timeout))
            } else {
                let mut c_control_stream =
                    time::timeout(timeout, TcpStream::connect(&config.path)).await??;
                c_control_stream.set_nodelay(true)?;
                // switch to TLS if required
                if let Some(ref builder) = tls_builder {
                    greeting.push(COMM_TLS);
                    trace!("Setting up TLS control connection");
                    time::timeout(
                        reduce_timeout(timeout, op_start),
                        c_control_stream.write_all(&greeting),
                    )
                    .await??;
                    let connector = TlsConnector::from(builder.build()?);
                    Box::new(SStream::new(
                        connector
                            .connect(path_domain.unwrap_or_default(), c_control_stream)
                            .await?,
                        timeout,
                    ))
                } else {
                    greeting.push(COMM_INSECURE);
                    trace!("Setting up insecure control connection");
                    time::timeout(
                        reduce_timeout(timeout, op_start),
                        c_control_stream.write_all(&greeting),
                    )
                    .await??;
                    Box::new(SStream::new(c_control_stream, timeout))
                }
            }
        };
        let mut buf: [u8; 2] = [0; 2];
        trace!("reading control header");
        control_stream
            .read_with_timeout(&mut buf, reduce_timeout(timeout, op_start))
            .await?;
        if buf != crate::CONTROL_HEADER {
            return Err(Error::invalid_data("Invalid control header"));
        }
        trace!("reading control protocol version");
        control_stream
            .read_with_timeout(&mut buf, reduce_timeout(timeout, op_start))
            .await?;
        if u16::from_le_bytes(buf) != crate::PROTOCOL_VERSION {
            return Err(Error::invalid_data("Protocol version unsupported"));
        }
        // Authenticate
        trace!("authenticating");
        let mut auth_buf = Vec::new();
        #[allow(clippy::cast_possible_truncation)]
        auth_buf.extend(
            (config.user.as_bytes().len() as u32 + config.password.as_bytes().len() as u32 + 1)
                .to_le_bytes(),
        );
        auth_buf.extend(config.user.as_bytes());
        auth_buf.push(0x00);
        auth_buf.extend(config.password.as_bytes());
        control_stream
            .write_with_timeout(&auth_buf, reduce_timeout(timeout, op_start))
            .await?;
        let mut token_buf: [u8; 32] = [0; 32];
        if control_stream
            .read_with_timeout(&mut token_buf, reduce_timeout(timeout, op_start))
            .await
            .is_err()
        {
            return Err(Error::access("Login failed"));
        }
        let (control_channel, rx) = async_channel::bounded::<Command>(config.queue_size);
        let connected = Arc::new(atomic::AtomicBool::new(true));
        let mut timeout_secs = timeout.as_secs();
        if timeout_secs > 255 {
            timeout_secs = 255;
        }
        // Connect data stream if required
        let (data_fut, data_channel) = if config.need_data_stream {
            let mut greeting = Vec::new();
            greeting.extend(crate::DATA_HEADER);
            trace!("connecting data stream to {}", config.path);
            let mut data_stream: Box<dyn StreamHandler> = {
                if is_unix {
                    let mut c_data_stream = time::timeout(
                        reduce_timeout(timeout, op_start),
                        UnixStream::connect(&config.path),
                    )
                    .await??;
                    greeting.push(COMM_INSECURE);
                    time::timeout(
                        reduce_timeout(timeout, op_start),
                        c_data_stream.write_all(&greeting),
                    )
                    .await??;
                    Box::new(SStream::new(c_data_stream, timeout))
                } else {
                    let mut c_data_stream = time::timeout(
                        reduce_timeout(timeout, op_start),
                        TcpStream::connect(&config.path),
                    )
                    .await??;
                    c_data_stream.set_nodelay(true)?;
                    // switch data stream to TLS if required
                    if let Some(ref builder) = tls_builder {
                        greeting.push(COMM_TLS);
                        trace!("Setting up TLS data connection");
                        time::timeout(
                            reduce_timeout(timeout, op_start),
                            c_data_stream.write_all(&greeting),
                        )
                        .await??;
                        let connector = TlsConnector::from(builder.build()?);
                        Box::new(SStream::new(
                            connector
                                .connect(path_domain.unwrap_or_default(), c_data_stream)
                                .await?,
                            timeout,
                        ))
                    } else {
                        greeting.push(COMM_INSECURE);
                        trace!("Setting up insecure data connection");
                        time::timeout(
                            reduce_timeout(timeout, op_start),
                            c_data_stream.write_all(&greeting),
                        )
                        .await??;
                        Box::new(SStream::new(c_data_stream, timeout))
                    }
                }
            };
            let mut buf: [u8; 2] = [0; 2];
            trace!("reading data header");
            data_stream
                .read_with_timeout(&mut buf, reduce_timeout(timeout, op_start))
                .await?;
            if buf != crate::DATA_HEADER {
                return Err(Error::invalid_data("Invalid data header"));
            }
            trace!("reading data protocol version");
            data_stream
                .read_with_timeout(&mut buf, reduce_timeout(timeout, op_start))
                .await?;
            if u16::from_le_bytes(buf) != crate::PROTOCOL_VERSION {
                return Err(Error::invalid_data("Protocol version unsupported"));
            }
            // Authenticate data stream with token
            data_stream
                .write_with_timeout(&token_buf, reduce_timeout(timeout, op_start))
                .await?;
            #[allow(clippy::cast_possible_truncation)]
            data_stream
                .write_with_timeout(&[timeout_secs as u8], reduce_timeout(timeout, op_start))
                .await?;
            let mut response_buf: [u8; 1] = [0];
            data_stream
                .read_with_timeout(&mut response_buf, reduce_timeout(timeout, op_start))
                .await?;
            if response_buf[0] != RESPONSE_OK {
                return Err(Error::access("Data stream authentication failed"));
            }
            let (tx, data_channel) = async_channel::bounded::<Message>(config.queue_size);
            let cc = control_channel.clone();
            let conn = connected.clone();
            // Spawn data task
            let data_fut = tokio::spawn(async move {
                let _r = Self::run_data_stream(data_stream, tx, cc).await;
                conn.store(false, atomic::Ordering::SeqCst);
            });
            (Some(data_fut), Some(data_channel))
        } else {
            (None, None)
        };
        // Remaining tasks
        trace!("initializing");
        let conn = connected.clone();
        // Spawn control task
        let path = config.path.clone();
        let beacon_interval = Duration::from_millis(timeout_secs * 1000 / 2);
        let control_fut = tokio::spawn(async move {
            let _r =
                Self::run_control_stream(control_stream, rx, &path, beacon_interval, timeout).await;
            conn.store(false, atomic::Ordering::SeqCst);
        });
        let cc = control_channel.clone();
        let conn = connected.clone();
        // Spawn control channel pinger
        let control_ping_fut = tokio::spawn(async move {
            loop {
                sleep(beacon_interval).await;
                if cc
                    .send(Command {
                        control_command: ControlCommand::Nop,
                        response_channel: None,
                    })
                    .await
                    .is_err()
                {
                    conn.store(false, atomic::Ordering::SeqCst);
                    break;
                }
            }
        });
        trace!("client connected");
        Ok(Self {
            control_fut,
            control_ping_fut,
            data_fut,
            control_channel,
            data_channel,
            connected,
            timeout,
        })
    }

    #[inline]
    pub fn is_connected(&self) -> bool {
        self.connected.load(atomic::Ordering::SeqCst)
    }

    #[inline]
    pub fn take_data_channel(&mut self) -> Option<async_channel::Receiver<Message>> {
        self.data_channel.take()
    }

    /// # Errors
    ///
    /// Will return Err on communcation errors
    pub async fn subscribe(&self, topic: String) -> Result<(), Error> {
        exec_command!(
            self.control_channel,
            ControlCommand::Subscribe(topic),
            self.timeout
        )
    }

    /// # Errors
    ///
    /// Will return Err on communcation errors
    pub async fn unsubscribe(&self, topic: String) -> Result<(), Error> {
        exec_command!(
            self.control_channel,
            ControlCommand::Unsubscribe(topic),
            self.timeout
        )
    }
    /// # Errors
    ///
    /// Will return Err on communcation errors
    pub async fn subscribe_bulk(&self, topics: Vec<String>) -> Result<(), Error> {
        exec_command!(
            self.control_channel,
            ControlCommand::SubscribeBulk(topics),
            self.timeout
        )
    }

    /// # Errors
    ///
    /// Will return Err on communcation errors
    pub async fn unsubscribe_bulk(&self, topics: Vec<String>) -> Result<(), Error> {
        exec_command!(
            self.control_channel,
            ControlCommand::UnsubscribeBulk(topics),
            self.timeout
        )
    }

    /// Message is always vec to avoid unnecessary data copy
    ///
    /// # Errors
    ///
    /// Will return Err on communcation errors
    pub async fn publish(
        &self,
        priority: u8,
        topic: String,
        message: Vec<u8>,
    ) -> Result<(), Error> {
        exec_command!(
            self.control_channel,
            ControlCommand::Publish(priority, topic, message),
            self.timeout
        )
    }

    /// # Errors
    ///
    /// Will return Err on communcation errors
    pub async fn publish_repl(
        &self,
        priority: u8,
        topic: Arc<String>,
        message: Arc<Vec<u8>>,
        timestamp: u64,
    ) -> Result<(), Error> {
        exec_command!(
            self.control_channel,
            ControlCommand::PublishRepl(priority, topic, message, timestamp),
            self.timeout
        )
    }

    /// # Errors
    ///
    /// Will return Err on communcation errors
    pub async fn bye(&self) -> Result<(), Error> {
        exec_command!(self.control_channel, ControlCommand::Bye, self.timeout)?;
        self.connected.store(false, atomic::Ordering::SeqCst);
        self.shutdown();
        Ok(())
    }

    async fn run_control_stream(
        mut control_stream: Box<dyn StreamHandler>,
        rx: async_channel::Receiver<Command>,
        path: &str,
        beacon_interval: Duration,
        timeout: Duration,
    ) -> Result<(), Error> {
        let mut response_buf: [u8; 1] = [0];
        let mut op_start = Instant::now();
        while let Ok(cmd) = rx.recv().await {
            if cmd.control_command == ControlCommand::Nop && op_start.elapsed() < beacon_interval {
                continue;
            }
            op_start = Instant::now();
            control_stream
                .write(&cmd.control_command.as_bytes())
                .await?;
            macro_rules! handle_err {
                ($code: expr) => {
                    let err = if $code == RESPONSE_ERR_ACCESS {
                        Error::access("Operation access denied")
                    } else {
                        Error::io(format!("Server response error {:x}", $code))
                    };
                    if let Some(ch) = cmd.response_channel {
                        let _r = ch.send(Err(err.clone()));
                    }
                    if err.kind() == ErrorKind::Io {
                        return Err(err);
                    }
                };
            }
            macro_rules! process_reply {
                () => {
                    control_stream
                        .read_with_timeout(&mut response_buf, reduce_timeout(timeout, op_start))
                        .await?;
                    let code = response_buf[0];
                    if code == RESPONSE_OK {
                        if let Some(ch) = cmd.response_channel {
                            let _r = ch.send(Ok(()));
                        }
                    } else {
                        handle_err!(code);
                    }
                };
            }
            match cmd.control_command {
                ControlCommand::Publish(_, _, ref message) => {
                    control_stream
                        .write_with_timeout(message, reduce_timeout(timeout, op_start))
                        .await?;
                    process_reply!();
                }
                ControlCommand::PublishRepl(priority, ref topic, ref message, timestamp) => {
                    control_stream
                        .read_with_timeout(&mut response_buf, reduce_timeout(timeout, op_start))
                        .await?;
                    let code = response_buf[0];
                    match code {
                        RESPONSE_OK_WAITING => {
                            let mut msg_buf: Vec<u8> = Vec::with_capacity(13);
                            msg_buf.push(priority);
                            msg_buf.extend(timestamp.to_le_bytes());
                            #[allow(clippy::cast_possible_truncation)]
                            msg_buf.extend((message.len() as u32).to_le_bytes());
                            control_stream
                                .write_with_timeout(&msg_buf, reduce_timeout(timeout, op_start))
                                .await?;
                            control_stream
                                .write_with_timeout(message, reduce_timeout(timeout, op_start))
                                .await?;
                            control_stream
                                .read_with_timeout(
                                    &mut response_buf,
                                    reduce_timeout(timeout, op_start),
                                )
                                .await?;
                            let code = response_buf[0];
                            if code == RESPONSE_OK {
                                if let Some(ch) = cmd.response_channel {
                                    let _r = ch.send(Ok(()));
                                }
                            } else {
                                handle_err!(code);
                            }
                        }
                        RESPONSE_NOT_REQUIRED => {
                            trace!("neighbor {} not requiers: {}", path, topic);
                            if let Some(ch) = cmd.response_channel {
                                let _r = ch.send(Ok(()));
                            }
                        }
                        _ => {
                            handle_err!(code);
                        }
                    }
                }
                _ => {
                    process_reply!();
                }
            }
        }
        Ok(())
    }

    async fn run_data_stream(
        mut data_stream: Box<dyn StreamHandler>,
        tx: async_channel::Sender<Message>,
        cc: async_channel::Sender<Command>,
    ) -> Result<(), Error> {
        let mut buf: [u8; 1] = [0];
        let timeout = data_stream.get_timeout();
        loop {
            let op_start = Instant::now();
            data_stream.read(&mut buf).await?;
            match buf[0] {
                OP_NOP => {
                    trace!("nop");
                }
                RESPONSE_OK => {
                    trace!("got message");
                    let mut prio: [u8; 1] = [crate::DEFAULT_PRIORITY];
                    data_stream
                        .read_with_timeout(&mut prio, reduce_timeout(timeout, op_start))
                        .await?;
                    let priority = prio[0];
                    let mut frame = data_stream
                        .read_frame_with_timeout(None, reduce_timeout(timeout, op_start))
                        .await?;
                    match Message::from_buf(&mut frame, priority) {
                        Ok(v) => {
                            trace!("message topic: {}", v.topic());
                            tx.send(v)
                                .await
                                .map_err(|_| Error::internal(ERR_COMMUNCATION_LOST))?;
                        }
                        Err(e) => {
                            trace!("invalid message: {}", e);
                        }
                    }
                }
                _ => {
                    let _r = cc
                        .send(Command {
                            control_command: ControlCommand::Bye,
                            response_channel: None,
                        })
                        .await;
                    return Err(Error::invalid_data("Invalid frame from the data channel"));
                }
            }
        }
    }

    fn shutdown(&self) {
        self.control_fut.abort();
        self.control_ping_fut.abort();
        self.data_fut.as_ref().map(JoinHandle::abort);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// UDP Client with optional AES-256-GCM encryption. One-way communcation only (no
/// acknowledgements)
#[allow(clippy::module_name_repetitions)]
pub struct UdpClient {
    socket: UdpSocket,
    login: Vec<u8>,
    password: Vec<u8>,
    encrypt: bool,
}

impl UdpClient {
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect(addr).await?;
        Ok(Self {
            socket,
            login: <_>::default(),
            password: <_>::default(),
            encrypt: false,
        })
    }
    pub fn with_auth(mut self, login: &str, password: &str) -> Self {
        self.login = login.as_bytes().to_vec();
        self.password = password.as_bytes().to_vec();
        self.encrypt = false;
        self
    }
    pub fn with_encryption_auth(mut self, login: &str, key: &[u8]) -> Self {
        self.login = login.as_bytes().to_vec();
        self.password = key.to_owned();
        self.encrypt = true;
        self
    }
    pub async fn publish(&self, topic: &str, message: &[u8]) -> Result<(), Error> {
        let mut buf = Vec::new();
        buf.extend(crate::CONTROL_HEADER);
        buf.extend(crate::PROTOCOL_VERSION.to_le_bytes());
        if self.encrypt {
            buf.push(crate::AUTH_KEY_AES256_GCM);
            buf.extend(&self.login);
            buf.push(0x00);
            let mut nonce = [0; 12];
            openssl::rand::rand_bytes(&mut nonce).map_err(Error::internal)?;
            let mut msg_buf = Vec::with_capacity(topic.len() + message.len() + 3);
            msg_buf.push(crate::OP_PUBLISH_NO_ACK);
            msg_buf.push(crate::DEFAULT_PRIORITY);
            msg_buf.extend(topic.as_bytes());
            msg_buf.push(0x00);
            msg_buf.extend(message);
            let mut digest = [0u8; 16];
            let encrypted = openssl::symm::encrypt_aead(
                openssl::symm::Cipher::aes_256_gcm(),
                &self.password,
                Some(&nonce),
                &[],
                &msg_buf,
                &mut digest,
            )
            .map_err(Error::io)?;
            buf.extend(&nonce);
            buf.extend(&encrypted);
            buf.extend(&digest);
        } else {
            buf.push(crate::AUTH_LOGIN_PASS);
            buf.extend(&self.login);
            buf.push(0x00);
            buf.extend(&self.password);
            buf.push(0x00);
            buf.push(crate::OP_PUBLISH_NO_ACK);
            buf.push(crate::DEFAULT_PRIORITY);
            buf.extend(topic.as_bytes());
            buf.push(0x00);
            buf.extend(message);
        }
        self.socket.send(&buf).await?;
        Ok(())
    }
}
