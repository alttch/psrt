use chrono::prelude::*;
use clap::Clap;
use colored::Colorize;
use log::{error, info, trace, warn};
use log::{Level, LevelFilter};
use once_cell::sync::{Lazy, OnceCell};
use psrt::acl::{self, ACL_DB};
use psrt::comm::{SStream, StreamHandler};
use psrt::is_unix_socket;
use psrt::pubsub::now_ns;
use psrt::pubsub::MessageFrame;
use psrt::pubsub::TOPIC_INVALID_SYMBOLS;
use psrt::pubsub::{ServerClient, ServerClientDB, ServerClientDBStats};
use psrt::reduce_timeout;
use psrt::token::Token;
use psrt::Error;
use psrt::COMM_INSECURE;
use psrt::COMM_TLS;
use psrt::DEFAULT_PRIORITY;
use psrt::OP_BYE;
use psrt::OP_NOP;
use psrt::OP_PUBLISH;
use psrt::OP_PUBLISH_REPL;
use psrt::OP_SUBSCRIBE;
use psrt::OP_UNSUBSCRIBE;
use psrt::RESPONSE_ERR;
use psrt::RESPONSE_ERR_ACCESS;
use psrt::RESPONSE_OK;
use psrt::{CONTROL_HEADER, DATA_HEADER};
use serde::{Deserialize, Serialize};
#[cfg(feature = "cluster")]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket, UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{Mutex, RwLock};
use tokio::time;
use tokio_native_tls::{native_tls, TlsAcceptor};

static ERR_INVALID_DATA_BLOCK: &str = "Invalid data block";
const MAX_AUTH_FRAME_SIZE: usize = 1024;
const DEFAULT_UDP_FRAME_SIZE: u16 = 4096;
const DEFAULT_TIMEOUT_SECS: f64 = 5.0;
const DEFAULT_STANDALONE_WORKERS: usize = 1;

static ALLOW_ANONYMOUS: atomic::AtomicBool = atomic::AtomicBool::new(false);
static MAX_PUB_SIZE: atomic::AtomicUsize = atomic::AtomicUsize::new(0);
static MAX_TOPIC_LENGTH: atomic::AtomicUsize = atomic::AtomicUsize::new(0);

static CONFIG_FILES: &[&str] = &["/etc/psrtd/config.yml", "/usr/local/etc/psrtd/config.yml"];

#[cfg(not(feature = "std-alloc"))]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[cfg(feature = "cluster")]
static APP_NAME: &str = "PSRT Enterprise";
#[cfg(not(feature = "cluster"))]
static APP_NAME: &str = "PSRT";

use stats::Counters;

mod eva_svc;

macro_rules! acl_dbm {
    () => {
        ACL_DB.write().await
    };
}

macro_rules! acl_db {
    () => {
        ACL_DB.read().await
    };
}

macro_rules! dbm {
    () => {
        DB.write()
    };
}

macro_rules! db {
    () => {
        DB.read()
    };
}

macro_rules! stats_counters {
    () => {
        STATS_COUNTERS.write()
    };
}

macro_rules! format_login {
    ($login: expr) => {
        if $login.is_empty() {
            "anonymous"
        } else {
            $login
        }
    };
}

// keep async as internal functions have async/io
static PASSWORD_DB: Lazy<RwLock<psrt::passwords::Passwords>> = Lazy::new(<_>::default);
static KEY_DB: Lazy<RwLock<psrt::keys::Keys>> = Lazy::new(<_>::default);
static DB: Lazy<parking_lot::RwLock<ServerClientDB>> = Lazy::new(<_>::default);
static STATS_COUNTERS: Lazy<parking_lot::RwLock<Counters>> = Lazy::new(<_>::default);
// keep async as it's used to block multiple term handlers
static PID_FILE: Lazy<Mutex<Option<String>>> = Lazy::new(<_>::default);
static HOST_NAME: OnceCell<String> = OnceCell::new();
static UPTIME: Lazy<Instant> = Lazy::new(Instant::now);
static UNIX_SOCKETS: Lazy<parking_lot::Mutex<BTreeSet<PathBuf>>> = Lazy::new(<_>::default);

macro_rules! respond_status {
    ($stream: expr, $status: expr) => {
        $stream.write(&[$status]).await?;
    };
}

macro_rules! respond_ok {
    ($stream: expr) => {
        respond_status!($stream, RESPONSE_OK);
    };
}

macro_rules! respond_err {
    ($stream: expr) => {
        respond_status!($stream, RESPONSE_ERR);
    };
}

macro_rules! respond_deny {
    ($stream: expr) => {
        respond_status!($stream, RESPONSE_ERR_ACCESS);
    };
}

#[derive(Serialize, Debug)]
pub struct ServerStatus {
    time: u64,
    uptime: u64,
    data_queue_size: usize,
    host: &'static str,
    version: String,
    counters: Counters,
    clients: ServerClientDBStats,
    #[cfg(feature = "cluster")]
    cluster: Option<BTreeMap<String, psrt::replication::NodeStatus>>,
    #[cfg(not(feature = "cluster"))]
    cluster: Option<()>,
}

#[allow(clippy::unused_async)]
async fn get_status() -> ServerStatus {
    let counters = { STATS_COUNTERS.read().clone() };
    let clients = { db!().get_stats() };
    #[cfg(feature = "cluster")]
    let cluster = psrt::replication::status().await;
    #[cfg(not(feature = "cluster"))]
    let cluster = None;
    ServerStatus {
        time: now_ns() / 1000,
        uptime: UPTIME.elapsed().as_secs(),
        version: psrt::VERSION.to_owned(),
        data_queue_size: psrt::pubsub::get_data_queue_size(),
        host: HOST_NAME.get().unwrap(),
        counters,
        clients,
        cluster,
    }
}

#[derive(Debug, Eq, PartialEq)]
enum StreamType {
    Control,
    Data,
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::mutable_key_type)]
async fn push_to_subscribers(
    subscribers: &BTreeSet<ServerClient>,
    priority: u8,
    topic: &str,
    message: Arc<Vec<u8>>,
    timestamp: u64,
) {
    let mut frame = Vec::with_capacity(6 + topic.as_bytes().len() + message.len());
    frame.push(RESPONSE_OK);
    frame.push(priority);
    frame.extend((topic.as_bytes().len() as u32 + message.len() as u32 + 1).to_le_bytes());
    frame.extend(topic.as_bytes());
    frame.push(0x00);
    let message = Arc::new(MessageFrame {
        timestamp: Some(timestamp),
        frame,
        data: Some(message),
    });
    for s in subscribers {
        let c = s.data_channel();
        if let Some(dc) = c.as_ref() {
            if dc.is_full() {
                warn!(
                    "queue is full ({}@{}), dropping data channel",
                    format_login!(s.login()),
                    s.addr()
                );
                dbm!().unregister_data_channel(s.token());
                s.abort_tasks();
            } else if let Err(e) = dc.send(message.clone()).await {
                error!("{} ({}@{})", e, format_login!(s.login()), s.addr());
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::mutable_key_type)]
async fn process_control(
    mut stream: Box<dyn StreamHandler>,
    client: ServerClient,
    addr: &str,
    acl: Arc<acl::Acl>,
    timeout: Duration,
) -> Result<(), Error> {
    loop {
        let op_start = Instant::now();
        let mut cmd: [u8; 1] = [0];
        stream.read(&mut cmd).await?;
        macro_rules! parse_topics {
            ($data: expr) => {{
                let mut sp = $data.split(|v| *v == 0x00);
                let mut topics = Vec::new();
                while let Some(t) = sp.next() {
                    if !t.is_empty() {
                        topics.push(std::str::from_utf8(t)?);
                    }
                }
                topics
            }};
        }
        match cmd[0] {
            OP_NOP => {
                trace!("client {}: OP_NOP", client);
                respond_ok!(stream);
            }
            OP_SUBSCRIBE => {
                trace!("client {}: OP_SUBSCRIBE", client);
                let data = stream
                    .read_frame_with_timeout(
                        Some(MAX_TOPIC_LENGTH.load(atomic::Ordering::SeqCst)),
                        reduce_timeout(timeout, op_start),
                    )
                    .await?;
                let topics = parse_topics!(data);
                let mut res = RESPONSE_OK;
                {
                    let mut db = dbm!();
                    for topic in topics {
                        if !acl.allow_read(topic) {
                            res = RESPONSE_ERR_ACCESS;
                            error!(
                                "sub access deneid for {} to {}@{}",
                                topic,
                                format_login!(client.login()),
                                addr
                            );
                            break;
                        }
                        if db.subscribe(topic, &client).is_err() {
                            res = RESPONSE_ERR;
                            break;
                        }
                        {
                            stats_counters!().count_sub_ops();
                        }
                    }
                }
                respond_status!(stream, res);
            }
            OP_UNSUBSCRIBE => {
                trace!("client {}: OP_UNSUBSCRIBE", client);
                let data = stream
                    .read_frame_with_timeout(
                        Some(MAX_TOPIC_LENGTH.load(atomic::Ordering::SeqCst)),
                        reduce_timeout(timeout, op_start),
                    )
                    .await?;
                let topics = parse_topics!(data);
                let mut success = true;
                {
                    let mut db = dbm!();
                    for topic in topics {
                        if db.unsubscribe(topic, &client).is_err() {
                            success = false;
                            break;
                        }
                        {
                            stats_counters!().count_sub_ops();
                        }
                    }
                }
                if success {
                    respond_ok!(stream);
                } else {
                    respond_err!(stream);
                }
            }
            OP_PUBLISH => {
                trace!("client {}: OP_PUBLISH", client);
                // use system time instead of monotonic to replicate it between nodes
                let timestamp = now_ns();
                let mut prio: [u8; 1] = [DEFAULT_PRIORITY];
                stream
                    .read_with_timeout(&mut prio, reduce_timeout(timeout, op_start))
                    .await?;
                let priority = prio[0];
                let mut buf = stream
                    .read_frame_with_timeout(
                        Some(MAX_PUB_SIZE.load(atomic::Ordering::SeqCst)),
                        reduce_timeout(timeout, op_start),
                    )
                    .await?;
                {
                    stats_counters!().count_pub_bytes(buf.len() as u64);
                };
                let npos = buf
                    .iter()
                    .position(|n| *n == 0)
                    .ok_or_else(|| Error::invalid_data(ERR_INVALID_DATA_BLOCK))?;
                let data = buf.split_off(npos + 1);
                buf.truncate(buf.len() - 1);
                if buf.is_empty() {
                    return Err(Error::invalid_data(ERR_INVALID_DATA_BLOCK));
                }
                // prepare topic before to send it formatted to all clients
                if let Ok(topic) = psrt::pubsub::prepare_topic(std::str::from_utf8(&buf)?) {
                    #[allow(clippy::redundant_slicing)]
                    if topic.contains(TOPIC_INVALID_SYMBOLS) {
                        return Err(Error::invalid_data(ERR_INVALID_DATA_BLOCK));
                    }
                    if acl.allow_write(&topic) {
                        let subscribers = { db!().get_subscribers(&topic) };
                        let data = Arc::new(data);
                        if !subscribers.is_empty() {
                            push_to_subscribers(
                                &subscribers,
                                priority,
                                &topic,
                                data.clone(),
                                timestamp,
                            )
                            .await;
                        }
                        #[cfg(feature = "cluster")]
                        psrt::replication::push(priority, &topic, data, timestamp).await;
                        respond_ok!(stream);
                    } else {
                        error!(
                            "pub access deneid for {} to {}@{}",
                            topic,
                            format_login!(client.login()),
                            addr
                        );
                        respond_deny!(stream);
                    }
                } else {
                    error!("{}: topic too deep", addr);
                    respond_err!(stream);
                }
            }
            OP_PUBLISH_REPL => {
                trace!("client {}: OP_PUBLISH_REPL", client);
                #[cfg(not(feature = "cluster"))]
                return Err(Error::invalid_data(
                    "Unsupported operation: OP_PUBLISH_REPL",
                ));
                #[cfg(feature = "cluster")]
                {
                    let topic_data = stream
                        .read_frame_with_timeout(
                            Some(MAX_TOPIC_LENGTH.load(atomic::Ordering::SeqCst)),
                            reduce_timeout(timeout, op_start),
                        )
                        .await?;
                    // do not prepare topic as it comes from another node
                    let topic = std::str::from_utf8(&topic_data)?;
                    if acl.is_replicator() {
                        let subscribers = { db!().get_subscribers(topic) };
                        if subscribers.is_empty() {
                            trace!("client: {} ({}) {} not required", client, addr, topic);
                            respond_status!(stream, psrt::RESPONSE_NOT_REQUIRED);
                        } else {
                            respond_status!(stream, psrt::RESPONSE_OK_WAITING);
                            let mut prio: [u8; 1] = [DEFAULT_PRIORITY];
                            stream
                                .read_with_timeout(&mut prio, reduce_timeout(timeout, op_start))
                                .await?;
                            let priority = prio[0];
                            let mut ts_buf: [u8; 8] = [0; 8];
                            stream
                                .read_with_timeout(&mut ts_buf, reduce_timeout(timeout, op_start))
                                .await?;
                            let timestamp = u64::from_le_bytes(ts_buf);
                            let data = stream
                                .read_frame_with_timeout(
                                    Some(MAX_PUB_SIZE.load(atomic::Ordering::SeqCst)),
                                    reduce_timeout(timeout, op_start),
                                )
                                .await?;
                            push_to_subscribers(
                                &subscribers,
                                priority,
                                topic,
                                Arc::new(data),
                                timestamp,
                            )
                            .await;
                            respond_ok!(stream);
                        }
                    } else {
                        error!(
                            "replication access deneid for {} to {}@{}",
                            topic,
                            format_login!(client.login()),
                            addr
                        );
                        respond_deny!(stream);
                    }
                }
            }
            OP_BYE => {
                trace!("client {}: OP_BYE", client);
                respond_ok!(stream);
                break;
            }
            _ => {
                return Err(Error::invalid_data(format!(
                    "Invalid operation: {}",
                    cmd[0]
                )));
            }
        }
    }
    Ok(())
}

async fn init_unix_stream(
    mut client_stream: UnixStream,
    timeout: Duration,
) -> Result<(Box<dyn StreamHandler>, StreamType), Error> {
    let mut greeting: [u8; 3] = [0; 3];
    time::timeout(timeout, client_stream.read_exact(&mut greeting)).await??;
    let header = &greeting[0..2];
    let stream_type = match greeting[0..2].try_into().unwrap() {
        CONTROL_HEADER => StreamType::Control,
        DATA_HEADER => StreamType::Data,
        _ => {
            return Err(Error::io(format!("Invalid greeting header: {:x?}", header)));
        }
    };
    let mut stream: Box<dyn StreamHandler> = Box::new(SStream::new(client_stream, timeout));
    let mut reply_header = Vec::new();
    reply_header.extend(&greeting[..2]);
    reply_header.extend(psrt::PROTOCOL_VERSION.to_le_bytes());
    stream.write(&reply_header).await?;
    Ok((stream, stream_type))
}

async fn init_tcp_stream(
    mut client_stream: TcpStream,
    addr: SocketAddr,
    timeout: Duration,
    acceptor: Option<TlsAcceptor>,
    allow_no_tls: bool,
) -> Result<(Box<dyn StreamHandler>, StreamType), Error> {
    client_stream.set_nodelay(true)?;
    let mut greeting: [u8; 3] = [0; 3];
    time::timeout(timeout, client_stream.read_exact(&mut greeting)).await??;
    let header = &greeting[0..2];
    let stream_type = match greeting[0..2].try_into().unwrap() {
        CONTROL_HEADER => StreamType::Control,
        DATA_HEADER => StreamType::Data,
        _ => {
            return Err(Error::io(format!("Invalid greeting header: {:x?}", header)));
        }
    };
    let mut stream: Box<dyn StreamHandler> = match greeting[2] {
        COMM_INSECURE => {
            if allow_no_tls {
                info!("{} using insecure connection", addr);
                Box::new(SStream::new(client_stream, timeout))
            } else {
                return Err(Error::io("Communication without TLS is forbidden"));
            }
        }
        COMM_TLS => {
            if let Some(a) = acceptor {
                info!("{} using TLS connection", addr);
                Box::new(SStream::new(a.accept(client_stream).await?, timeout))
            } else {
                return Err(Error::io("TLS is not configured"));
            }
        }
        v => {
            return Err(Error::io(format!("Invalid comm mode requested: {}", v)));
        }
    };
    let mut reply_header = Vec::new();
    reply_header.extend(&greeting[..2]);
    reply_header.extend(psrt::PROTOCOL_VERSION.to_le_bytes());
    stream.write(&reply_header).await?;
    Ok((stream, stream_type))
}

/// # Errors
///
/// Will return Err if no password fine defined or unable to read
#[inline]
pub async fn authenticate(login: &str, password: &str) -> bool {
    PASSWORD_DB.read().await.verify(login, password)
}

#[inline]
pub async fn get_acl(login: &str) -> Option<Arc<acl::Acl>> {
    acl_db!().get_acl(if login.is_empty() { "_" } else { login })
}

async fn handle_unix_stream(
    client_stream: UnixStream,
    path: &str,
    timeout: Duration,
) -> Result<bool, Error> {
    let (mut stream, st) = init_unix_stream(client_stream, timeout).await?;
    if st == StreamType::Data {
        launch_data_stream(stream, timeout).await?;
        return Ok(false);
    }
    let frame = stream.read_frame(Some(MAX_AUTH_FRAME_SIZE)).await?;
    let mut sp = frame.splitn(2, |n| *n == 0);
    let login = std::str::from_utf8(
        sp.next()
            .ok_or_else(|| Error::invalid_data(ERR_INVALID_DATA_BLOCK))?,
    )?;
    let client = { dbm!().register_client(login, path) }?;
    stream.write(client.token_as_bytes()).await?;
    let result = process_control(
        stream,
        client.clone(),
        path,
        acl::Acl::new_full().into(),
        timeout,
    )
    .await;
    {
        dbm!().unregister_client(&client);
        client.abort_tasks();
    }
    result?;
    Ok(true)
}

async fn handle_tcp_stream(
    client_stream: TcpStream,
    addr: SocketAddr,
    timeout: Duration,
    acceptor: Option<TlsAcceptor>,
    allow_no_tls: bool,
) -> Result<bool, Error> {
    let (mut stream, st) =
        init_tcp_stream(client_stream, addr, timeout, acceptor, allow_no_tls).await?;
    if st == StreamType::Data {
        launch_data_stream(stream, timeout).await?;
        return Ok(false);
    }
    let frame = stream.read_frame(Some(MAX_AUTH_FRAME_SIZE)).await?;
    let mut sp = frame.splitn(2, |n| *n == 0);
    let login = std::str::from_utf8(
        sp.next()
            .ok_or_else(|| Error::invalid_data(ERR_INVALID_DATA_BLOCK))?,
    )?;
    let password = std::str::from_utf8(
        sp.next()
            .ok_or_else(|| Error::invalid_data(ERR_INVALID_DATA_BLOCK))?,
    )?;
    if login.is_empty() && password.is_empty() {
        if ALLOW_ANONYMOUS.load(atomic::Ordering::SeqCst) {
            trace!("Anonymous logged in from {}", addr);
        } else {
            trace!("Anonymous access denied from {}", addr);
            return Err(Error::access(format!(
                "anonymous login failed from {}",
                addr
            )));
        }
    } else if authenticate(login, password).await {
        trace!("User {} logged in from {}", login, addr);
    } else {
        trace!("Access denied for {} from {}", login, addr);
        return Err(Error::access(format!(
            "login failed for {} from {}",
            login, addr
        )));
    }
    let Some(acl) = get_acl(login).await else {
        return Err(Error::access(format!(
            "No ACL for {}",
            format_login!(login)
        )));
    };
    let client = { dbm!().register_client(login, &addr.to_string()) }?;
    stream.write(client.token_as_bytes()).await?;
    let result = process_control(stream, client.clone(), &addr.to_string(), acl, timeout).await;
    {
        dbm!().unregister_client(&client);
        client.abort_tasks();
    }
    result?;
    Ok(true)
}

#[allow(clippy::cast_precision_loss)]
async fn launch_data_stream(
    mut stream: Box<dyn StreamHandler>,
    timeout: Duration,
) -> Result<(), Error> {
    let mut buf: [u8; 32] = [0; 32];
    let op_start = Instant::now();
    stream.read(&mut buf).await?;
    let token = Token::from(buf);
    let mut buf: [u8; 1] = [0];
    stream
        .read_with_timeout(&mut buf, reduce_timeout(timeout, op_start))
        .await?;
    let (tx, rx) = async_channel::bounded(psrt::pubsub::get_data_queue_size());
    {
        let res = { dbm!().register_data_channel(&token, tx) };
        match res {
            Ok((channel, client)) => {
                respond_ok!(stream);
                let beacon_freq = u64::from(buf[0]) * 1000 / 2;
                trace!(
                    "client {} reported timeout: {}, setting beacon freq to {} ms",
                    token,
                    buf[0],
                    beacon_freq
                );
                let beacon_interval = Duration::from_millis(beacon_freq);
                let empty_message = Arc::new(MessageFrame {
                    timestamp: None,
                    frame: vec![OP_NOP],
                    data: None,
                });
                let pinger_fut = tokio::spawn(async move {
                    loop {
                        time::sleep(beacon_interval).await;
                        if channel.send(empty_message.clone()).await.is_err() {
                            break;
                        }
                    }
                });
                client.register_task(pinger_fut);
                let username = format_login!(client.login()).to_owned();
                let addr = client.addr().to_owned();
                let data_fut = tokio::spawn(async move {
                    if let Err(e) = handle_data_stream(
                        stream,
                        &token,
                        rx,
                        timeout,
                        beacon_interval,
                        &username,
                        &addr,
                    )
                    .await
                    {
                        error!("data stream {}@{} error {}", username, addr, e);
                        dbm!().unregister_data_channel(&token);
                    }
                });
                client.register_task(data_fut);
            }
            Err(e) => {
                respond_err!(stream);
                return Err(e);
            }
        };
    }
    Ok(())
}

async fn handle_data_stream(
    mut stream: Box<dyn StreamHandler>,
    token: &Token,
    rx: async_channel::Receiver<Arc<MessageFrame>>,
    timeout: Duration,
    beacon_interval: Duration,
    username: &str,
    addr: &str,
) -> Result<(), Error> {
    macro_rules! eof_is_ok {
        ($result: expr) => {
            if let Err(e) = $result {
                if e.kind() == psrt::ErrorKind::Eof {
                    return Ok(());
                }
                return Err(e.into());
            }
        };
    }
    let latency_warn = psrt::pubsub::get_latency_warn();
    let mut last_command = Instant::now();
    while let Ok(message) = rx.recv().await {
        if message.frame[0] == OP_NOP {
            if last_command.elapsed() < beacon_interval {
                continue;
            }
            eof_is_ok!(stream.write(&[OP_NOP]).await);
        } else {
            trace!("Sending message_frame to {}", token);
            let op_start = Instant::now();
            eof_is_ok!(stream.write(&message.frame).await);
            if let Some(data) = message.data.as_ref() {
                eof_is_ok!(
                    stream
                        .write_with_timeout(data, reduce_timeout(timeout, op_start))
                        .await
                );
            }
            if let Some(timestamp) = message.timestamp {
                #[allow(clippy::cast_possible_truncation)]
                let latency_mks = ((now_ns() - timestamp) / 1000) as u32;
                {
                    stats_counters!().count_sent_bytes(
                        (message.frame.len() + message.data.as_ref().map_or(0, |v| v.len())) as u64,
                        latency_mks,
                    );
                };
                trace!("latency: {} \u{3bc}s", latency_mks);
                if latency_mks > latency_warn {
                    warn!(
                        "WARNING: high latency: {} \u{3bc}s topic {} ({}@{})",
                        latency_mks,
                        // will not panic as the topic is already verified
                        std::str::from_utf8(
                            message.frame[6..].splitn(2, |n| *n == 0).next().unwrap()
                        )
                        .unwrap(),
                        username,
                        addr
                    );
                }
            }
        }
        last_command = Instant::now();
    }
    Ok(())
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ConfigCluster {
    config: String,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ConfigAuth {
    allow_anonymous: bool,
    password_file: Option<String>,
    key_file: Option<String>,
    acl: String,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum StringOrList {
    Single(String),
    Multiple(Vec<String>),
}

impl Default for StringOrList {
    fn default() -> Self {
        Self::Multiple(Vec::new())
    }
}

impl From<StringOrList> for Vec<String> {
    fn from(l: StringOrList) -> Self {
        match l {
            StringOrList::Single(s) => vec![s],
            StringOrList::Multiple(v) => v,
        }
    }
}

impl<'a> From<&'a StringOrList> for Vec<&'a str> {
    fn from(l: &'a StringOrList) -> Self {
        match l {
            StringOrList::Single(ref s) => vec![s.as_str()],
            StringOrList::Multiple(ref v) => v.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ConfigProto {
    #[serde(default)]
    bind: StringOrList,
    bind_udp: StringOrList,
    udp_frame_size: Option<u16>,
    timeout: Option<f64>,
    tls_pkcs12: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    #[serde(default)]
    fips: bool,
    allow_no_tls: bool,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ConfigServer {
    workers: Option<usize>,
    latency_warn: u32,
    data_queue: usize,
    max_topic_depth: usize,
    max_pub_size: usize,
    max_topic_length: usize,
    pid_file: Option<String>,
    bind_stats: Option<String>,
    license: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Config {
    server: ConfigServer,
    proto: ConfigProto,
    auth: ConfigAuth,
    cluster: Option<ConfigCluster>,
}

#[derive(Clap)]
#[clap(version = psrt::VERSION, author = psrt::AUTHOR, name = APP_NAME)]
#[allow(clippy::struct_excessive_bools)]
struct Opts {
    #[clap(short = 'C', long = "config")]
    config_file: Option<String>,
    #[clap(short = 'v', about = "Verbose logging")]
    verbose: bool,
    #[clap(long = "log-syslog", about = "Force log to syslog")]
    log_syslog: bool,
    #[clap(short = 'd', about = "Run in the background")]
    daemonize: bool,
    #[clap(
        long = "eva-svc",
        about = "Run as EVA ICS v4 service (all other arguments are ignored)"
    )]
    eva_svc: bool,
}

struct SimpleLogger;

impl log::Log for SimpleLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let s = format!(
                "{}  {}",
                Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
                record.args()
            );
            println!(
                "{}",
                match record.level() {
                    Level::Trace => s.black().dimmed(),
                    Level::Debug => s.dimmed(),
                    Level::Warn => s.yellow().bold(),
                    Level::Error => s.red(),
                    Level::Info => s.normal(),
                }
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: SimpleLogger = SimpleLogger;

fn set_verbose_logger(filter: LevelFilter) {
    log::set_logger(&LOGGER)
        .map(|()| log::set_max_level(filter))
        .unwrap();
}

#[allow(clippy::too_many_lines)]
async fn launch_server(
    config: Config,
    listeners: Listeners,
    timeout: Duration,
    tls_identity: Option<native_tls::Identity>,
    mut replication_configs: Option<Vec<psrt::client::Config>>,
    create_pid_file: bool,
    _license: Option<String>,
) -> Result<(), Error> {
    let allow_no_tls = config.proto.allow_no_tls;
    let acceptor = if let Some(identity) = tls_identity {
        Some(TlsAcceptor::from(native_tls::TlsAcceptor::new(identity)?))
    } else {
        None
    };
    let udp_frame_size = config
        .proto
        .udp_frame_size
        .unwrap_or(DEFAULT_UDP_FRAME_SIZE);
    if create_pid_file {
        let pid = std::process::id().to_string();
        let pid_file = config.server.pid_file.expect("PID file not specified");
        info!("creating pid file {}, PID: {}", pid_file, pid);
        {
            PID_FILE.lock().await.replace(pid_file.clone());
        }
        tokio::fs::write(&pid_file, pid).await?;
    }
    #[cfg(feature = "cluster")]
    if let Some(configs) = replication_configs.take() {
        psrt::replication::start(configs, _license).await;
    }
    #[cfg(not(feature = "cluster"))]
    if let Some(_configs) = replication_configs.take() {
        warn!("cluster feature is disabled");
    }
    for (listener, path) in listeners.unix {
        tokio::spawn(handle_unix_listener(
            listener,
            path.to_string_lossy().to_string(),
            timeout,
        ));
    }
    for listener in listeners.tcp {
        tokio::spawn(handle_tcp_listener(
            listener,
            acceptor.clone(),
            timeout,
            allow_no_tls,
        ));
    }
    for listener in listeners.udp {
        tokio::spawn(handle_udp_listener(listener, udp_frame_size));
    }
    let sleep_step = std::time::Duration::from_secs(1);
    loop {
        tokio::time::sleep(sleep_step).await;
    }
}

async fn handle_unix_listener(listener: UnixListener, path: String, timeout: Duration) {
    loop {
        let path_c = path.clone();
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(async move {
                    info!("Client connected: {}", path_c);
                    match handle_unix_stream(stream, &path_c, timeout).await {
                        Ok(true) => info!("Client disconnected: {}", path_c),
                        Ok(false) => info!("Data stream launched for {}", path_c),
                        Err(e) => error!("Client {} error. {}", path_c, e),
                    }
                });
            }
            Err(e) => {
                error!("{}", e);
            }
        }
    }
}

async fn handle_tcp_listener(
    listener: TcpListener,
    acceptor: Option<TlsAcceptor>,
    timeout: Duration,
    allow_no_tls: bool,
) {
    loop {
        let acc = acceptor.clone();
        match listener.accept().await {
            Ok((stream, addr)) => {
                tokio::spawn(async move {
                    info!("Client connected: {}", addr);
                    match handle_tcp_stream(stream, addr, timeout, acc, allow_no_tls).await {
                        Ok(true) => info!("Client disconnected: {}", addr),
                        Ok(false) => info!("Data stream launched for {}", addr),
                        Err(e) => error!("Client {} error. {}", addr, e),
                    }
                });
            }
            Err(e) => {
                error!("{}", e);
            }
        }
    }
}

async fn handle_udp_listener(listener: UdpSocket, udp_frame_size: u16) {
    let mut buf = vec![0_u8; udp_frame_size as usize];
    loop {
        match listener.recv_from(&mut buf).await {
            Ok((len, addr)) => {
                trace!("udp packet {} bytes from {}", len, addr);
                let frame: Vec<u8> = buf[..len].to_vec();
                let ack_code = match process_udp_packet(frame).await {
                    Ok(true) => Some(RESPONSE_OK),
                    Err((e, need_ack)) => {
                        error!("udp packet from {} error: {}", addr, e);
                        if need_ack {
                            if e.kind() == psrt::ErrorKind::AccessDenied {
                                Some(RESPONSE_ERR_ACCESS)
                            } else {
                                Some(RESPONSE_ERR)
                            }
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(code) = ack_code {
                    let mut buf = CONTROL_HEADER.to_vec();
                    buf.extend(psrt::PROTOCOL_VERSION.to_le_bytes());
                    buf.push(code);
                    if let Err(e) = listener.send_to(&buf, addr).await {
                        error!("{}", e);
                    }
                }
            }
            Err(e) => {
                error!("udp socket error: {}", e);
            }
        }
    }
}

#[allow(clippy::mutable_key_type)]
async fn process_udp_block(
    login: &str,
    password: Option<&str>,
    block: &[u8],
    timestamp: u64,
) -> Result<bool, (Error, bool)> {
    let mut sp = block.splitn(2, |n: &u8| *n == 0);
    let buf = sp
        .next()
        .ok_or_else(|| (Error::invalid_data("data block missing"), false))?;
    if buf.len() < 3 {
        return Err((Error::invalid_data("invalid data block"), false));
    }
    let need_ack = match buf[0] {
        OP_PUBLISH => true,
        psrt::OP_PUBLISH_NO_ACK => false,
        v => {
            return Err((
                Error::invalid_data(format!("invalid opration: {:x?}", v)),
                false,
            ));
        }
    };
    let priority = buf[1];
    let topic = std::str::from_utf8(&buf[2..]).map_err(|e| (e.into(), need_ack))?;
    let data = sp
        .next()
        .ok_or_else(|| (Error::invalid_data("data missing"), need_ack))?;
    if let Some(password) = password {
        if login.is_empty() && password.is_empty() {
            if !ALLOW_ANONYMOUS.load(atomic::Ordering::SeqCst) {
                return Err((Error::access("anonymous access failed"), need_ack));
            }
        } else if !authenticate(login, password).await {
            return Err((Error::access("authentication failed"), need_ack));
        }
    }
    let Some(acl) = get_acl(login).await else {
        return Err((
            Error::access(format!("No ACL for {}", format_login!(login))),
            need_ack,
        ));
    };
    let topic = psrt::pubsub::prepare_topic(topic).map_err(|e| (e, need_ack))?;
    #[allow(clippy::redundant_slicing)]
    if topic.contains(TOPIC_INVALID_SYMBOLS) {
        return Err((Error::invalid_data(ERR_INVALID_DATA_BLOCK), need_ack));
    }
    if !acl.allow_write(&topic) {
        return Err((
            Error::access(format!("pub access denied for {}", topic)),
            need_ack,
        ));
    }
    let subscribers = { db!().get_subscribers(&topic) };
    let data = Arc::new(data.to_vec());
    if !subscribers.is_empty() {
        push_to_subscribers(&subscribers, priority, &topic, data.clone(), timestamp).await;
    }
    #[cfg(feature = "cluster")]
    psrt::replication::push(priority, &topic, data, timestamp).await;
    Ok(need_ack)
}

/// bool in replies = true if ack required
async fn process_udp_packet(frame: Vec<u8>) -> Result<bool, (Error, bool)> {
    let timestamp = now_ns();
    if frame.len() < 5 {
        return Err((Error::invalid_data("packet too small"), false));
    }
    if frame[..2] != CONTROL_HEADER {
        return Err((Error::invalid_data("invalid header"), false));
    }
    if u16::from_le_bytes([frame[2], frame[3]]) != psrt::PROTOCOL_VERSION {
        return Err((Error::invalid_data("unsupported protocol version"), false));
    }
    let etp = psrt::keys::EncryptionType::from_byte(frame[4]).map_err(|e| (e, false))?;
    trace!("UDP packet encryption: {:?}", etp);
    let mut sp = frame[5..].splitn(2, |n: &u8| *n == 0);
    let login = std::str::from_utf8(
        sp.next()
            .ok_or_else(|| (Error::invalid_data("login / key id missing"), false))?,
    )
    .map_err(|e| (e.into(), false))?;
    if etp.need_decrypt() {
        let block = KEY_DB
            .read()
            .await
            .auth_and_decr(
                sp.next()
                    .ok_or_else(|| (Error::invalid_data("encryption block missing"), false))?,
                if login.is_empty() { "_" } else { login },
                etp,
            )
            .map_err(|e| (e, false))?;
        process_udp_block(login, None, &block, timestamp).await
    } else {
        let block = sp
            .next()
            .ok_or_else(|| (Error::invalid_data("invalid packet format"), false))?;
        let mut sp = block.splitn(2, |n: &u8| *n == 0);
        let password = std::str::from_utf8(
            sp.next()
                .ok_or_else(|| (Error::invalid_data("password missing"), false))?,
        )
        .map_err(|e| (e.into(), false))?;
        process_udp_block(
            login,
            Some(password),
            sp.next()
                .ok_or_else(|| (Error::invalid_data("data missing"), false))?,
            timestamp,
        )
        .await
    }
}

async fn terminate(allow_log: bool) {
    let pid_file = PID_FILE.lock().await;
    #[cfg(feature = "cluster")]
    psrt::replication::stop().await;
    // do not log anything on C-c
    if let Some(f) = pid_file.as_ref() {
        if allow_log {
            trace!("removing pid file {}", f);
        }
        let _r = std::fs::remove_file(f);
    }
    if allow_log {
        info!("terminating");
    }
    cleanup();
    std::process::exit(0);
}

macro_rules! handle_term_signal {
    ($kind: expr, $allow_log: expr) => {
        tokio::spawn(async move {
            trace!("starting handler for {:?}", $kind);
            loop {
                match signal($kind) {
                    Ok(mut v) => {
                        v.recv().await;
                    }
                    Err(e) => {
                        error!("Unable to bind to signal {:?}: {}", $kind, e);
                        break;
                    }
                }
                // do not log anything on C-c
                if $allow_log {
                    trace!("got termination signal");
                }
                terminate($allow_log).await
            }
        });
    };
}

fn format_path(path: &str, cdir: &Path) -> PathBuf {
    if path.starts_with(&['/', '.'][..]) {
        Path::new(path).to_owned()
    } else {
        let mut p = cdir.to_owned();
        p.push(path);
        p
    }
}

async fn prepare_tls_identity(
    config_proto: &ConfigProto,
    cdir: &Path,
) -> Option<native_tls::Identity> {
    if let Some(ref tls_pkcs12) = config_proto.tls_pkcs12 {
        let p12_path = format_path(tls_pkcs12, cdir);
        info!("loading TLS PKCS12 {}", p12_path.to_string_lossy());
        let p12 = tokio::fs::read(p12_path)
            .await
            .expect("Unable to load TLS PKCS12");
        Some(native_tls::Identity::from_pkcs12(&p12, "").unwrap())
    } else if let Some(ref tls_cert) = config_proto.tls_cert {
        let cert_path = format_path(tls_cert, cdir);
        info!("loading TLS cert {}", cert_path.to_string_lossy());
        let cert = tokio::fs::read(cert_path)
            .await
            .expect("Unable to load TLS cert");
        let key_path = format_path(
            config_proto
                .tls_key
                .as_ref()
                .expect("TLS key not specified"),
            cdir,
        );
        info!("loading TLS key {}", key_path.to_string_lossy());
        let key = tokio::fs::read(key_path)
            .await
            .expect("Unable to load TLS key");
        let priv_key = openssl::pkey::PKey::private_key_from_pem(&key).unwrap();
        Some(
            native_tls::Identity::from_pkcs8(&cert, &priv_key.private_key_to_pem_pkcs8().unwrap())
                .unwrap(),
        )
    } else {
        None
    }
}

async fn prepare_replication_configs(
    config_cluster: Option<&ConfigCluster>,
    cdir: &Path,
    timeout: Duration,
    queue_size: usize,
) -> Option<Vec<psrt::client::Config>> {
    if let Some(c) = config_cluster {
        let fname = format_path(&c.config, cdir);
        info!("loading cluster config {}", fname.to_string_lossy());
        let cfg = tokio::fs::read_to_string(fname).await.unwrap();
        let mut cfgs: Vec<psrt::client::Config> = serde_yaml::from_str(&cfg).unwrap();
        let mut configs = Vec::new();
        while !cfgs.is_empty() {
            let mut c = cfgs.remove(0);
            if let Some(tls_ca) = c.tls_ca() {
                c.update_tls_ca(
                    tokio::fs::read_to_string(format_path(tls_ca, cdir))
                        .await
                        .unwrap(),
                );
            }
            c = c.set_queue_size(queue_size);
            c = c.set_timeout(timeout);
            configs.push(c);
        }
        Some(configs)
    } else {
        None
    }
}

async fn load_license(license: Option<&str>, cdir: &Path) -> Option<String> {
    if let Some(f) = license {
        let fname = format_path(f, cdir);
        info!("reading license file {}", fname.to_string_lossy());
        Some(tokio::fs::read_to_string(fname).await.unwrap())
    } else {
        None
    }
}

macro_rules! reload_db {
    ($db: expr) => {
        if let Err(e) = $db.reload().await {
            error!("Unable to load config: {}", e);
        }
    };
}

struct AdditinalConfigs {
    tls_identity: Option<native_tls::Identity>,
    replication_configs: Option<Vec<psrt::client::Config>>,
    license: Option<String>,
}

impl AdditinalConfigs {
    async fn load(config: &Config, cdir: &Path, timeout: Duration) -> Self {
        let tls_identity: Option<native_tls::Identity> =
            prepare_tls_identity(&config.proto, cdir).await;
        let replication_configs = prepare_replication_configs(
            config.cluster.as_ref(),
            cdir,
            timeout,
            config.server.data_queue,
        )
        .await;
        let license = load_license(config.server.license.as_deref(), cdir).await;
        Self {
            tls_identity,
            replication_configs,
            license,
        }
    }
}

struct Listeners {
    tcp: Vec<TcpListener>,
    udp: Vec<UdpSocket>,
    unix: Vec<(UnixListener, PathBuf)>,
}

impl Listeners {
    async fn create(config_proto: &ConfigProto, cdir: &Path) -> Result<Self, Error> {
        let mut tcp = Vec::new();
        let mut unix = Vec::new();
        let mut udp = Vec::new();
        for s_bind in Vec::from(&config_proto.bind) {
            if is_unix_socket(s_bind) {
                let bind = format_path(s_bind, cdir);
                info!("binding UNIX socket to: {}", bind.to_string_lossy());
                let _ = tokio::fs::remove_file(&bind).await;
                let sock = UnixListener::bind(&bind)?;
                let mut permissions = tokio::fs::metadata(&bind).await?.permissions();
                permissions.set_mode(0o770);
                tokio::fs::set_permissions(&bind, permissions).await?;
                UNIX_SOCKETS.lock().insert(bind.clone());
                unix.push((sock, bind.clone()));
            } else {
                info!("binding TCP socket to: {}", s_bind);
                let sock = TcpListener::bind(s_bind).await?;
                tcp.push(sock);
            }
        }
        for bind in Vec::from(&config_proto.bind_udp) {
            info!("binding UDP socket to: {}", bind);
            let sock = UdpSocket::bind(bind).await?;
            udp.push(sock);
        }
        Ok(Self { tcp, udp, unix })
    }
}

async fn load_auth(config_auth: &ConfigAuth, cdir: &Path) {
    if let Some(ref f) = config_auth.password_file {
        let mut passwords = PASSWORD_DB.write().await;
        let password_file = format_path(f, cdir);
        passwords.set_password_file(&password_file);
        reload_db!(passwords);
    }
    if let Some(ref f) = config_auth.key_file {
        let mut keys = KEY_DB.write().await;
        let key_file = format_path(f, cdir);
        keys.set_key_file(&key_file);
        reload_db!(keys);
    }
}

async fn load_acl(config_auth: &ConfigAuth, cdir: &Path) {
    let mut acl = ACL_DB.write().await;
    acl.set_path(&format_path(&config_auth.acl, cdir));
    acl.reload().await.expect("unable to reload ACL file");
}

async fn launch(
    config: Config,
    ac: AdditinalConfigs,
    listeners: Listeners,
    cdir: PathBuf,
    timeout: Duration,
    workers: usize,
    standalone: bool,
) {
    psrt::pubsub::set_latency_warn(config.server.latency_warn);
    psrt::pubsub::set_data_queue_size(config.server.data_queue);
    ALLOW_ANONYMOUS.store(config.auth.allow_anonymous, atomic::Ordering::SeqCst);
    MAX_TOPIC_LENGTH.store(config.server.max_topic_length, atomic::Ordering::SeqCst);
    MAX_PUB_SIZE.store(config.server.max_pub_size, atomic::Ordering::SeqCst);
    psrt::pubsub::set_max_topic_depth(config.server.max_topic_depth);
    if standalone {
        load_acl(&config.auth, &cdir).await;
        load_auth(&config.auth, &cdir).await;
        handle_term_signal!(SignalKind::interrupt(), false);
        handle_term_signal!(SignalKind::terminate(), true);
        tokio::spawn(async move {
            let kind = SignalKind::hangup();
            trace!("starting handler for {:?}", kind);
            loop {
                match signal(kind) {
                    Ok(mut v) => {
                        v.recv().await;
                    }
                    Err(e) => {
                        error!("Unable to bind to signal {:?}: {}", kind, e);
                        break;
                    }
                }
                trace!("got hangup signal, reloading configs");
                {
                    if let Err(e) = acl_dbm!().reload().await {
                        error!("ACL reload failed: {}", e);
                    }
                    {
                        reload_db!(PASSWORD_DB.write().await);
                    }
                    {
                        reload_db!(KEY_DB.write().await);
                    }
                }
            }
        });
    }
    if let Some(ref bind_stats) = config.server.bind_stats {
        stats::start(bind_stats);
    }
    info!(
        "starting server, workers: {}, timeout: {:?}",
        workers, timeout
    );
    if let Err(e) = launch_server(
        config,
        listeners,
        timeout,
        ac.tls_identity,
        ac.replication_configs,
        standalone,
        ac.license,
    )
    .await
    {
        error!("{}", e);
        cleanup();
        std::process::exit(1);
    }
}

fn main() {
    let opts = Opts::parse();
    if let Ok(name) = hostname::get() {
        HOST_NAME.set(name.to_string_lossy().to_string()).unwrap();
    } else {
        HOST_NAME.set("unknown".to_owned()).unwrap();
    }
    if opts.eva_svc {
        eva_sdk::service::svc_launch(eva_svc::main).unwrap();
    } else {
        // standalone-specific
        if opts.verbose {
            set_verbose_logger(LevelFilter::Trace);
        } else if (!opts.daemonize
            || std::env::var("DISABLE_SYSLOG").unwrap_or_else(|_| "0".to_owned()) == "1")
            && !opts.log_syslog
        {
            set_verbose_logger(LevelFilter::Info);
        } else {
            let formatter = syslog::Formatter3164 {
                facility: syslog::Facility::LOG_USER,
                hostname: None,
                process: "psrtd".into(),
                pid: 0,
            };
            match syslog::unix(formatter) {
                Ok(logger) => {
                    log::set_boxed_logger(Box::new(syslog::BasicLogger::new(logger)))
                        .map(|()| log::set_max_level(LevelFilter::Info))
                        .unwrap();
                }
                Err(_) => {
                    set_verbose_logger(LevelFilter::Info);
                }
            }
        }
        info!("version: {}", psrt::VERSION);
        let (cfg, mut cfile) = if let Some(cfile) = opts.config_file {
            info!("using config: {}", cfile);
            (
                std::fs::read_to_string(&cfile).expect("unable to read config"),
                cfile,
            )
        } else {
            let mut cfg = None;
            let mut path: Option<String> = None;
            for cfile in CONFIG_FILES {
                match std::fs::read_to_string(cfile) {
                    Ok(v) => {
                        cfg = Some(v);
                        path = Some((*cfile).to_string());
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        panic!("Unable to read {}: {}", cfile, e);
                    }
                }
            }
            (
                cfg.unwrap_or_else(|| {
                    panic!("Unable to read config ({})", CONFIG_FILES.join(", "))
                }),
                path.unwrap(),
            )
        };
        if !cfile.starts_with(&['.', '/'][..]) {
            cfile = format!("./{}", cfile);
        }
        let cdir = Path::new(&cfile)
            .parent()
            .expect("Unable to get config dir")
            .canonicalize()
            .expect("Unable to parse config path");
        let config: Config = serde_yaml::from_str(&cfg).unwrap();
        if config.proto.fips {
            #[cfg(not(feature = "openssl-vendored"))]
            openssl::fips::enable(true).expect("Can not enable OpenSSL FIPS 140");
            #[cfg(not(feature = "openssl-vendored"))]
            info!("OpenSSL FIPS 140 enabled");
            #[cfg(feature = "openssl-vendored")]
            panic!("FIPS can not be enabled, consider using a native OS distribution");
        }
        if opts.daemonize {
            if let Ok(fork::Fork::Child) = fork::daemon(true, false) {
                std::process::exit(0);
            }
        }
        // end standalone-specific
        let timeout = Duration::from_secs_f64(config.proto.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));
        let workers = config.server.workers.unwrap_or(DEFAULT_STANDALONE_WORKERS);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let ac = AdditinalConfigs::load(&config, &cdir, timeout).await;
            let listeners = Listeners::create(&config.proto, &cdir).await.unwrap();
            launch(config, ac, listeners, cdir, timeout, workers, true).await;
        });
    }
}

fn cleanup() {
    for sock in &*UNIX_SOCKETS.lock() {
        let _ = std::fs::remove_file(sock);
    }
}

mod stats {
    #![allow(arithmetic_overflow)]
    use hyper::service::{make_service_fn, service_fn};
    use hyper::{Body, Method, Request, Response, Server, StatusCode};
    use std::convert::Infallible;
    use std::net::SocketAddr;

    static HTML_STATS: &str = include_str!("stats.html");
    static JS_CHART_MIN: &str = include_str!("chart.min.js");

    macro_rules! http_error {
        ($status: expr) => {
            Ok(Response::builder()
                .status($status)
                .body(Body::from(String::new()))
                .unwrap())
        };
    }

    macro_rules! response {
        ($code: expr, $content: expr) => {
            Ok(Response::builder()
                .status($code)
                .body(Body::from($content))
                .unwrap())
        };
    }

    use serde::Serialize;
    #[derive(Serialize, Debug, Clone, Default)]
    pub struct Counters {
        // counters
        c_sub_ops: u64,
        c_pub_messages: u64,
        c_pub_bytes: u64,
        c_sent_messages: u64,
        c_sent_bytes: u64,
        c_sent_latency: u64,
    }

    impl Counters {
        #[inline]
        pub fn count_sub_ops(&mut self) {
            self.c_sub_ops += 1;
        }
        #[inline]
        pub fn count_pub_bytes(&mut self, size: u64) {
            self.c_pub_messages += 1;
            self.c_pub_bytes += size;
        }
        #[inline]
        pub fn count_sent_bytes(&mut self, size: u64, latency: u32) {
            self.c_sent_messages += 1;
            self.c_sent_bytes += size;
            self.c_sent_latency += u64::from(latency);
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn handler(req: Request<Body>) -> Result<Response<Body>, Infallible> {
        let (parts, _body) = req.into_parts();
        if parts.method == Method::GET {
            let path = parts.uri.path();
            let credentials = if let Some(authorization) = parts.headers.get("authorization") {
                if let Ok(auth) = authorization.to_str() {
                    let mut sp = auth.splitn(2, ' ');
                    let scheme = sp.next().unwrap();
                    if let Some(params) = sp.next() {
                        if scheme.to_lowercase() == "basic" {
                            match base64::decode(params) {
                                Ok(ref v) => match std::str::from_utf8(v) {
                                    Ok(s) => {
                                        let mut sp = s.splitn(2, ':');
                                        let username = sp.next().unwrap();
                                        if let Some(password) = sp.next() {
                                            Some((username.to_owned(), password.to_owned()))
                                        } else {
                                            return response!(
                                                StatusCode::BAD_REQUEST,
                                                "Basic authorization error: password not specified"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        return response!(
                                            StatusCode::BAD_REQUEST,
                                            format!("Unable to parse credentials string: {}", e)
                                        );
                                    }
                                },
                                Err(e) => {
                                    return response!(
                                        StatusCode::BAD_REQUEST,
                                        format!("Unable to decode credentials: {}", e)
                                    );
                                }
                            }
                        } else {
                            None
                        }
                    } else {
                        return response!(StatusCode::BAD_REQUEST, "Invalid authorization header");
                    }
                } else {
                    return response!(
                        StatusCode::BAD_REQUEST,
                        "Unable to decode authorization header"
                    );
                }
            } else {
                None
            };
            let mut authorized = false;
            if let Some((ref login, ref password)) = credentials {
                if super::authenticate(login, password).await {
                    if let Some(acl) = super::get_acl(login).await {
                        if acl.is_admin() {
                            authorized = true;
                        }
                    }
                }
            } else if let Some(acl) = super::get_acl("_").await {
                if acl.is_admin() {
                    authorized = true;
                }
            }
            if !authorized {
                return Ok(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header("www-authenticate", "Basic realm=\"?\"")
                    .body(Body::from(String::new()))
                    .unwrap());
            }
            match path {
                "/status" => {
                    let status = super::get_status().await;
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&status).unwrap()))
                        .unwrap())
                }
                "/chart.min.js" => Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/javascript; charset=utf-8")
                    .body(Body::from(JS_CHART_MIN))
                    .unwrap()),
                "/" => Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/html; charset=utf-8")
                    .body(Body::from(HTML_STATS))
                    .unwrap()),
                _ => {
                    http_error!(StatusCode::NOT_FOUND)
                }
            }
        } else {
            http_error!(StatusCode::METHOD_NOT_ALLOWED)
        }
    }

    pub fn start(path: &str) {
        let addr: SocketAddr = path.parse().unwrap();
        log::info!("binding stats server to: {}", addr);
        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handler)) });
        tokio::spawn(async move {
            loop {
                let server = Server::bind(&addr).serve(make_svc);
                let _r = server.await;
            }
        });
    }
}
