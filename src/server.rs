#[macro_use]
extern crate lazy_static;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{Mutex, RwLock};
use tokio::time;

use rustls_pemfile::{certs, rsa_private_keys};
use tokio_rustls::rustls::{self, Certificate, PrivateKey};
use tokio_rustls::TlsAcceptor;

use psrt::comm::SStream;

#[cfg(feature = "cluster")]
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic;
use std::sync::Arc;
use std::time::{Duration, Instant};

use psrt::pubsub::{ServerClient, ServerClientDB, ServerClientDBStats};
use psrt::reduce_timeout;
use psrt::token::Token;

use psrt::RESPONSE_ERR;
use psrt::RESPONSE_ERR_ACCESS;
use psrt::RESPONSE_OK;

use psrt::DEFAULT_PRIORITY;

use psrt::Error;
use psrt::OP_BYE;
use psrt::OP_NOP;
use psrt::OP_PUBLISH;
use psrt::OP_PUBLISH_REPL;
use psrt::OP_SUBSCRIBE;
use psrt::OP_UNSUBSCRIBE;

use psrt::{CONTROL_HEADER, DATA_HEADER};

use psrt::acl::{self, ACL_DB};
use psrt::COMM_INSECURE;
use psrt::COMM_TLS;

use psrt::pubsub::now_ns;
use psrt::pubsub::MessageFrame;
use psrt::pubsub::TOPIC_INVALID_SYMBOLS;

use chrono::prelude::*;
use clap::Clap;
use colored::Colorize;
use log::{error, info, trace, warn};
use log::{Level, LevelFilter};
use serde::{Deserialize, Serialize};

static ERR_INVALID_DATA_BLOCK: &str = "Invalid data block";
const MAX_AUTH_FRAME_SIZE: usize = 1024;
const DEFAULT_UDP_FRAME_SIZE: u16 = 4096;

static ALLOW_ANONYMOUS: atomic::AtomicBool = atomic::AtomicBool::new(false);
static MAX_PUB_SIZE: atomic::AtomicUsize = atomic::AtomicUsize::new(0);
static MAX_TOPIC_LENGTH: atomic::AtomicUsize = atomic::AtomicUsize::new(0);

static CONFIG_FILES: &[&str] = &["/etc/psrtd/config.yml", "/usr/local/etc/psrtd/config.yml"];

#[cfg(feature = "cluster")]
static APP_NAME: &str = "PSRT Enterprise";
#[cfg(not(feature = "cluster"))]
static APP_NAME: &str = "PSRT";

use stats::Counters;

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
        DB.write().await
    };
}

macro_rules! db {
    () => {
        DB.read().await
    };
}

macro_rules! stats_counters {
    () => {
        STATS_COUNTERS.write().await
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

lazy_static! {
    static ref DB: RwLock<ServerClientDB> = RwLock::new(<_>::default());
    static ref PASSWORD_DB: RwLock<psrt::passwords::Passwords> = RwLock::new(<_>::default());
    static ref KEY_DB: RwLock<psrt::keys::Keys> = RwLock::new(<_>::default());
    static ref STATS_COUNTERS: RwLock<Counters> = RwLock::new(Counters::new());
    static ref PID_FILE: Mutex<Option<String>> = Mutex::new(None);
    static ref HOST_NAME: RwLock<String> = RwLock::new("unknown".to_owned());
    static ref UPTIME: Instant = Instant::now();
}

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
    host: String,
    version: String,
    counters: Counters,
    clients: ServerClientDBStats,
    #[cfg(feature = "cluster")]
    cluster: Option<BTreeMap<String, psrt::replication::NodeStatus>>,
    #[cfg(not(feature = "cluster"))]
    cluster: Option<bool>,
}

pub async fn get_status() -> ServerStatus {
    let counters = { stats_counters!().clone() };
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
        host: HOST_NAME.read().await.clone(),
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
async fn push_to_subscribers(
    subscribers: &HashSet<ServerClient>,
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
        let c = s.data_channel.read().await;
        if let Some(dc) = c.as_ref() {
            if let Err(e) = dc.send(message.clone()).await {
                error!("{}", e);
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn process_control(
    mut stream: SStream,
    client: ServerClient,
    addr: SocketAddr,
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
                        if db.subscribe(topic, client.clone()).is_err() {
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
                        if db.unsubscribe(topic, client.clone()).is_err() {
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
                    if topic.contains(&TOPIC_INVALID_SYMBOLS[..]) {
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

async fn init_stream(
    mut client_stream: TcpStream,
    addr: SocketAddr,
    timeout: Duration,
    acceptor: Option<TlsAcceptor>,
    allow_no_tls: bool,
) -> Result<(SStream, StreamType), Error> {
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
    let mut stream = match greeting[2] {
        COMM_INSECURE => {
            if allow_no_tls {
                info!("{} using insecure connection", addr);
                SStream::new(client_stream, timeout)
            } else {
                return Err(Error::io("Communication without TLS is forbidden"));
            }
        }
        COMM_TLS => {
            if let Some(a) = acceptor {
                info!("{} using TLS connection", addr);
                SStream::new_tls_server(a.accept(client_stream).await?, timeout)
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

async fn handle_stream(
    client_stream: TcpStream,
    addr: SocketAddr,
    timeout: Duration,
    acceptor: Option<TlsAcceptor>,
    allow_no_tls: bool,
) -> Result<bool, Error> {
    let (mut stream, st) =
        init_stream(client_stream, addr, timeout, acceptor, allow_no_tls).await?;
    if st == StreamType::Data {
        launch_data_stream(stream, addr, timeout).await?;
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
    let acl = if let Some(acl) = get_acl(login).await {
        acl
    } else {
        return Err(Error::access(format!(
            "No ACL for {}",
            format_login!(login)
        )));
    };
    let client = { dbm!().register_client(login) };
    stream.write(client.token_as_bytes()).await?;
    let result = process_control(stream, client.clone(), addr, acl, timeout).await;
    {
        dbm!().unregister_client(&client);
        client.abort_tasks().await;
    }
    result?;
    Ok(true)
}

#[allow(clippy::cast_precision_loss)]
#[allow(clippy::too_many_lines)]
async fn launch_data_stream(
    mut stream: SStream,
    addr: SocketAddr,
    timeout: Duration,
) -> Result<(), Error> {
    let mut buf: [u8; 32] = [0; 32];
    let op_start = Instant::now();
    stream.read(&mut buf).await?;
    let token = Token::from(buf);
    let (tx, rx) = async_channel::bounded(psrt::pubsub::get_data_queue_size());
    let mut buf: [u8; 1] = [0];
    let latency_warn = psrt::pubsub::get_latency_warn();
    stream
        .read_with_timeout(&mut buf, reduce_timeout(timeout, op_start))
        .await?;
    {
        match dbm!().register_data_channel(&token, tx).await {
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
                let mut tasks = client.tasks.write().await;
                let username = format_login!(client.login()).to_owned();
                let pinger_fut = tokio::spawn(async move {
                    loop {
                        time::sleep(beacon_interval).await;
                        if channel.send(empty_message.clone()).await.is_err() {
                            break;
                        }
                    }
                    Ok(())
                });
                tasks.push(pinger_fut);
                let data_fut = tokio::spawn(async move {
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
                            eof_is_ok!(stream.write(&*message.frame).await);
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
                                        (message.frame.len()
                                            + message.data.as_ref().map_or(0, |v| v.len()))
                                            as u64,
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
                                            message.frame[6..]
                                                .splitn(2, |n| *n == 0)
                                                .next()
                                                .unwrap()
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
                });
                tasks.push(data_fut);
            }
            Err(e) => {
                respond_err!(stream);
                return Err(e);
            }
        };
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
    aes_nonce: Option<String>,
    acl: String,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ConfigProto {
    bind: String,
    bind_udp: Option<String>,
    udp_frame_size: Option<u16>,
    timeout: f64,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    allow_no_tls: bool,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ConfigServer {
    workers: usize,
    latency_warn: u32,
    data_queue: usize,
    max_topic_depth: usize,
    max_pub_size: usize,
    max_topic_length: usize,
    pid_file: String,
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
struct Opts {
    #[clap(short = 'C', long = "config")]
    config_file: Option<String>,
    #[clap(short = 'v', about = "Verbose logging")]
    verbose: bool,
    #[clap(long = "log-syslog", about = "Force log to syslog")]
    log_syslog: bool,
    #[clap(short = 'd', about = "Run in the background")]
    daemonize: bool,
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

async fn run_server(
    config: &Config,
    timeout: Duration,
    tls_config: Option<rustls::ServerConfig>,
    mut replication_configs: Option<Vec<psrt::client::Config>>,
    _license: Option<String>,
) -> Result<(), Error> {
    let allow_no_tls = config.proto.allow_no_tls;
    let acceptor = tls_config.map(|c| TlsAcceptor::from(Arc::new(c)));
    info!("binding to: {}", config.proto.bind);
    let listener = TcpListener::bind(&config.proto.bind).await?;
    let pid = std::process::id().to_string();
    if let Ok(name) = hostname::get() {
        if let Some(name) = name.to_str() {
            *HOST_NAME.try_write().unwrap() = name.to_owned();
        }
    }
    info!(
        "starting server, workers: {}, timeout: {:?}",
        config.server.workers, timeout
    );
    info!("creating pid file {}, PID: {}", config.server.pid_file, pid);
    {
        PID_FILE
            .lock()
            .await
            .replace(config.server.pid_file.clone());
    }
    tokio::fs::write(&config.server.pid_file, pid).await?;
    #[cfg(feature = "cluster")]
    if let Some(configs) = replication_configs.take() {
        psrt::replication::start(configs, _license).await;
    }
    #[cfg(not(feature = "cluster"))]
    if let Some(_configs) = replication_configs.take() {
        warn!("cluster feature is disabled");
    }
    if let Some(ref bind_udp) = config.proto.bind_udp {
        let udp_frame_size = config
            .proto
            .udp_frame_size
            .unwrap_or(DEFAULT_UDP_FRAME_SIZE);
        info!(
            "binding UDP socket to: {}, max frame size: {}",
            bind_udp, udp_frame_size
        );
        let udp_sock = UdpSocket::bind(bind_udp).await?;
        tokio::spawn(async move {
            let mut buf = vec![0_u8; udp_frame_size as usize];
            loop {
                match udp_sock.recv_from(&mut buf).await {
                    Ok((len, addr)) => {
                        trace!("udp packet {} bytes from {}", len, addr);
                        let frame: Vec<u8> = buf[..len].iter().copied().collect();
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
                            buf.extend(&psrt::PROTOCOL_VERSION.to_le_bytes());
                            buf.push(code);
                            if let Err(e) = udp_sock.send_to(&buf, addr).await {
                                error!("{}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("udp socket error: {}", e);
                    }
                }
            }
        });
    }
    loop {
        let acc = acceptor.clone();
        match listener.accept().await {
            Ok((stream, addr)) => {
                tokio::spawn(async move {
                    info!("Client connected: {}", addr);
                    match handle_stream(stream, addr, timeout, acc, allow_no_tls).await {
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
    let acl = if let Some(acl) = get_acl(login).await {
        acl
    } else {
        return Err((
            Error::access(format!("No ACL for {}", format_login!(login))),
            need_ack,
        ));
    };
    let topic = psrt::pubsub::prepare_topic(topic).map_err(|e| (e, need_ack))?;
    #[allow(clippy::redundant_slicing)]
    if topic.contains(&TOPIC_INVALID_SYMBOLS[..]) {
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
        let _r = std::fs::remove_file(&f);
    }
    if allow_log {
        info!("terminating");
    }
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

fn load_certs(path: &str) -> Result<Vec<Certificate>, Error> {
    certs(&mut std::io::BufReader::new(
        std::fs::File::open(path)
            .map_err(|e| Error::io(format!("Unable to open {}: {}", path, e)))?,
    ))
    .map_err(|_| Error::invalid_data("invalid cert"))
    .map(|mut certs| certs.drain(..).map(Certificate).collect())
}

fn load_keys(path: &str) -> Result<Vec<PrivateKey>, Error> {
    rsa_private_keys(&mut std::io::BufReader::new(
        std::fs::File::open(path)
            .map_err(|e| Error::io(format!("Unable to open {}: {}", path, e)))?,
    ))
    .map_err(|_| Error::invalid_data("invalid key"))
    .map(|mut keys| keys.drain(..).map(PrivateKey).collect())
}

#[allow(clippy::too_many_lines)]
fn main() {
    let opts = Opts::parse();
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
        (std::fs::read_to_string(&cfile).unwrap(), cfile)
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
            cfg.unwrap_or_else(|| panic!("Unable to read config ({})", CONFIG_FILES.join(", "))),
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
    macro_rules! format_path {
        ($path: expr) => {
            if $path.starts_with(&['/', '.'][..]) {
                $path.to_string()
            } else {
                format!("{}/{}", cdir.to_str().unwrap(), $path)
            }
        };
    }
    let config: Config = serde_yaml::from_str(&cfg).unwrap();
    let tls_key = config.proto.tls_key.clone();
    let tls_config = if let Some(ref tls_cert) = config.proto.tls_cert {
        let c = format_path!(tls_cert);
        info!("loading TLS cert {}", c);
        let key_path = format_path!(tls_key.as_ref().expect("TLS key not specified"));
        let certs = load_certs(&c).unwrap();
        info!("loading TLS key {}", key_path);
        let mut keys = load_keys(&key_path).unwrap();
        if certs.is_empty() {
            panic!("Unable to load TLS certs");
        }
        if keys.is_empty() {
            panic!("Unable to load TLS keys");
        }
        let tls_config = rustls::ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(certs, keys.remove(0))
            .unwrap();
        Some(tls_config)
    } else {
        None
    };
    let timeout = Duration::from_secs_f64(config.proto.timeout);
    let replication_configs = config.cluster.as_ref().map(|c| {
        let fname = format_path!(c.config);
        info!("loading cluster config {}", fname);
        let cfg = std::fs::read_to_string(fname).unwrap();
        let mut cfgs: Vec<psrt::client::Config> = serde_yaml::from_str(&cfg).unwrap();
        let mut configs = Vec::new();
        while !cfgs.is_empty() {
            let mut c = cfgs.remove(0);
            #[allow(mutable_borrow_reservation_conflict)]
            if let Some(tls_ca) = c.tls_ca() {
                c.update_tls_ca(std::fs::read_to_string(format_path!(tls_ca)).unwrap());
            }
            c = c.set_queue_size(config.server.data_queue);
            c = c.set_timeout(timeout);
            configs.push(c);
        }
        configs
    });
    let license = config.server.license.as_ref().map(|f| {
        let fname = format_path!(f);
        info!("reading license file {}", fname);
        std::fs::read_to_string(fname).unwrap()
    });
    psrt::pubsub::set_latency_warn(config.server.latency_warn);
    psrt::pubsub::set_data_queue_size(config.server.data_queue);
    ALLOW_ANONYMOUS.store(config.auth.allow_anonymous, atomic::Ordering::SeqCst);
    MAX_TOPIC_LENGTH.store(config.server.max_topic_length, atomic::Ordering::SeqCst);
    MAX_PUB_SIZE.store(config.server.max_pub_size, atomic::Ordering::SeqCst);
    psrt::pubsub::set_max_topic_depth(config.server.max_topic_depth);
    if opts.daemonize {
        if let Ok(fork::Fork::Child) = fork::daemon(true, false) {
            std::process::exit(0);
        }
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.server.workers)
        .enable_all()
        .build()
        .unwrap();
    macro_rules! reload_db {
        ($db: expr) => {
            if let Err(e) = $db.reload().await {
                error!("Unable to load config: {}", e);
            }
        };
    }
    rt.block_on(async move {
        {
            let mut acl = ACL_DB.write().await;
            acl.set_path(&format_path!(config.auth.acl));
            acl.reload().await.unwrap();
        }
        if let Some(ref f) = config.auth.password_file {
            let mut passwords = PASSWORD_DB.write().await;
            let password_file = format_path!(f.clone());
            passwords.set_password_file(&password_file);
            reload_db!(passwords);
        }
        if let Some(ref f) = config.auth.key_file {
            let mut keys = KEY_DB.write().await;
            let key_file = format_path!(f.clone());
            keys.set_key_file(&key_file);
            keys.set_nonce(config.auth.aes_nonce.as_ref().map(|h| {
                hex::decode(h)
                    .expect("unable to parse nonce")
                    .try_into()
                    .expect("invalid nonce size (12 bytes required)")
            }));
            reload_db!(keys);
        }
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
        if let Some(ref bind_stats) = config.server.bind_stats {
            stats::start(bind_stats).await;
        }
        if let Err(e) = run_server(&config, timeout, tls_config, replication_configs, license).await
        {
            error!("{}", e);
            std::process::exit(1);
        }
    });
}

mod stats {
    #![allow(arithmetic_overflow)]
    use hyper::service::{make_service_fn, service_fn};
    use hyper::{Body, Method, Request, Response, Server, StatusCode};
    use std::convert::Infallible;
    use std::net::SocketAddr;

    const HTML_STATS: &str = include_str!("stats.html");
    const JS_CHART_MIN: &str = include_str!("chart.min.js");

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
    #[derive(Serialize, Debug, Clone)]
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
        pub fn new() -> Self {
            Self {
                c_sub_ops: 0,
                c_pub_messages: 0,
                c_pub_bytes: 0,
                c_sent_messages: 0,
                c_sent_bytes: 0,
                c_sent_latency: 0,
            }
        }
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
                    .body(Body::from("".to_owned()))
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

    pub async fn start(path: &str) {
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
