use crate::token::Token;
use crate::Error;
use log::trace;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic;
use std::sync::Arc;
use std::sync::Mutex;
use submap::SubMap;
use tokio::task::JoinHandle;

pub const TOPIC_INVALID_SYMBOLS: &[char] = &['#', '+'];

static LATENCY_WARN: atomic::AtomicU32 = atomic::AtomicU32::new(0);
static DATA_QUEUE_SIZE: atomic::AtomicUsize = atomic::AtomicUsize::new(0);
static MAX_TOPIC_DEPTH: atomic::AtomicUsize = atomic::AtomicUsize::new(0);

pub fn set_latency_warn(value: u32) {
    LATENCY_WARN.store(value, atomic::Ordering::SeqCst);
}

pub fn set_data_queue_size(value: usize) {
    DATA_QUEUE_SIZE.store(value, atomic::Ordering::SeqCst);
}

pub fn set_max_topic_depth(depth: usize) {
    MAX_TOPIC_DEPTH.store(depth, atomic::Ordering::SeqCst);
}

#[inline]
pub fn get_latency_warn() -> u32 {
    LATENCY_WARN.load(atomic::Ordering::SeqCst)
}

#[inline]
pub fn get_data_queue_size() -> usize {
    DATA_QUEUE_SIZE.load(atomic::Ordering::SeqCst)
}

pub struct MessageFrame {
    pub timestamp: Option<u64>,     // used for analytics only
    pub frame: Vec<u8>,             // packed RESPONSE_OK, priority and len
    pub data: Option<Arc<Vec<u8>>>, // message body
}

#[derive(Debug)]
pub struct ServerClientData {
    login: String,
    addr: SocketAddr,
    token: Arc<Token>,
    data_channel: Mutex<Option<async_channel::Sender<Arc<MessageFrame>>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl ServerClientData {
    #[inline]
    pub fn token_as_bytes(&self) -> &[u8] {
        self.token.as_bytes()
    }
    /// # Panics
    ///
    /// Will panic if the mutex is poisoned
    #[inline]
    pub fn data_channel(&self) -> Option<async_channel::Sender<Arc<MessageFrame>>> {
        self.data_channel.lock().unwrap().clone()
    }
    #[inline]
    pub fn login(&self) -> &str {
        &self.login
    }
    #[inline]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    /// # Panics
    ///
    /// Will panic if the mutex is poisoned
    pub fn abort_tasks(&self) {
        let mut tasks = self.tasks.lock().unwrap();
        for task in tasks.iter() {
            task.abort();
        }
        tasks.clear();
    }
    /// # Panics
    ///
    /// Will panic if the mutex is poisoned
    pub fn register_task(&self, task: JoinHandle<()>) {
        self.tasks.lock().unwrap().push(task);
    }
}

impl Drop for ServerClientData {
    fn drop(&mut self) {
        self.abort_tasks();
    }
}

impl PartialEq for ServerClientData {
    fn eq(&self, other: &Self) -> bool {
        self.token == other.token
    }
}

impl fmt::Display for ServerClientData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.token)
    }
}

impl Eq for ServerClientData {}

impl Hash for ServerClientData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.token.hash(state);
    }
}

pub type ServerClient = Arc<ServerClientData>;

#[derive(Debug, Serialize)]
pub struct ServerClientDBStats {
    subscription_count: usize,
    client_count: usize,
}

#[derive(Debug)]
pub struct ServerClientDB {
    submap: SubMap<ServerClient>,
    clients_by_token: HashMap<Arc<Token>, ServerClient>,
}

impl Default for ServerClientDB {
    fn default() -> Self {
        Self {
            submap: SubMap::new().match_any("+").wildcard("#"),
            clients_by_token: <_>::default(),
        }
    }
}

/// # Errors
///
/// Will return Err if the topic is invalid
pub fn prepare_topic(topic: &str) -> Result<String, Error> {
    let mut result = topic.to_owned();
    while result.contains("//") {
        result = result.replace("//", "/");
    }
    if result.starts_with('/') {
        result = result[1..].to_owned();
    }
    if result.matches('/').count() > MAX_TOPIC_DEPTH.load(atomic::Ordering::SeqCst) {
        Err(Error::invalid_data("the topic is too deep"))
    } else {
        Ok(result)
    }
}

impl ServerClientDB {
    /// # Errors
    ///
    /// Will return Err if the token is not registered
    ///
    /// # Panics
    ///
    /// Will panic if the mutex is poisoned
    pub fn register_data_channel(
        &mut self,
        token: &Token,
        channel: async_channel::Sender<Arc<MessageFrame>>,
    ) -> Result<(async_channel::Sender<Arc<MessageFrame>>, ServerClient), Error> {
        if let Some(ref mut client) = self.clients_by_token.get_mut(token) {
            let mut dc = client.data_channel.lock().unwrap();
            if dc.is_some() {
                trace!("duplicate data channel request for {}, refusing", token);
                return Err(Error::access("Data channel is already registered"));
            }
            trace!("data channel registered for {}", token);
            dc.replace(channel.clone());
            Ok((channel, client.clone()))
        } else {
            trace!("data channel access denied for {}", token);
            Err(Error::access("data channel access denied"))
        }
    }
    /// # Panics
    ///
    /// Will panic if the mutex is poisoned
    pub fn unregister_data_channel(&mut self, token: &Token) {
        if let Some(ref mut client) = self.clients_by_token.get_mut(token) {
            let mut dc = client.data_channel.lock().unwrap();
            dc.take();
        }
    }
    pub fn get_stats(&self) -> ServerClientDBStats {
        ServerClientDBStats {
            subscription_count: self.submap.subscription_count(),
            client_count: self.submap.client_count(),
        }
    }
    pub fn register_client(&mut self, login: &str, addr: SocketAddr) -> ServerClient {
        trace!("registering new client");
        loop {
            let client = Arc::new(ServerClientData {
                token: <_>::default(),
                login: login.to_owned(),
                addr,
                data_channel: <_>::default(),
                tasks: <_>::default(),
            });
            if self.submap.register_client(&client) {
                self.clients_by_token
                    .insert(client.token.clone(), client.clone());
                trace!("client registered: {}", client);
                break client;
            }
        }
    }
    pub fn unregister_client(&mut self, client: &ServerClient) {
        trace!("unregistering: {}", client);
        self.submap.unregister_client(client);
        self.clients_by_token.remove(&client.token);
        trace!("client unregistered: {}", client);
    }
    /// # Errors
    ///
    /// Will return Err if the topic is invalid
    pub fn subscribe(&mut self, topic: &str, client: &ServerClient) -> Result<(), Error> {
        trace!("subscribing {} to {}", client, topic);
        self.submap.subscribe(&prepare_topic(topic)?, client);
        trace!("client subscribed: {} to {}", client, topic);
        Ok(())
    }
    /// # Errors
    ///
    /// Will return Err if the topic is invalid
    pub fn unsubscribe(&mut self, topic: &str, client: &ServerClient) -> Result<(), Error> {
        trace!("subscribing {} from {}", client, topic);
        self.submap.unsubscribe(&prepare_topic(topic)?, client);
        trace!("client unsubscribed: {} from {}", client, topic);
        Ok(())
    }
    #[allow(clippy::mutable_key_type)]
    pub fn get_subscribers(&self, topic: &str) -> HashSet<ServerClient> {
        trace!("getting subscribers for topic: {}", topic);
        self.submap.get_subscribers(topic)
    }
}

#[allow(clippy::cast_sign_loss)]
/// # Panics
///
/// Will panic if system clock is not available
pub fn now_ns() -> u64 {
    let t = nix::time::clock_gettime(nix::time::ClockId::CLOCK_REALTIME).unwrap();
    t.tv_sec() as u64 * 1_000_000_000 + t.tv_nsec() as u64
}
