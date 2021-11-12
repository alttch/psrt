use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::Split;
use std::sync::atomic;
use std::sync::Arc;

use serde::Serialize;

use log::trace;
use rand::Rng;

use crate::Error;

const CLIENT_NOT_REG_ERR: &str = "Client not registered";

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

pub fn get_latency_warn() -> u32 {
    LATENCY_WARN.load(atomic::Ordering::SeqCst)
}

pub fn get_data_queue_size() -> usize {
    DATA_QUEUE_SIZE.load(atomic::Ordering::SeqCst)
}

pub fn set_max_topic_depth(depth: usize) {
    MAX_TOPIC_DEPTH.store(depth, atomic::Ordering::SeqCst);
}

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub struct Token([u8; 32]);

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

pub struct MessageFrame {
    pub timestamp: Option<u64>,     // used for analytics only
    pub frame: Vec<u8>,             // packed RESPONSE_OK, priority and len
    pub data: Option<Arc<Vec<u8>>>, // message body
}

#[derive(Debug)]
pub struct ServerClientData {
    login: String,
    token: Token,
    pub data_channel: RwLock<Option<async_channel::Sender<Arc<MessageFrame>>>>,
    pub tasks: RwLock<Vec<JoinHandle<Result<(), Error>>>>,
}

impl ServerClientData {
    #[inline]
    pub fn token_as_bytes(&self) -> &[u8] {
        &self.token.0
    }
    #[inline]
    pub fn login(&self) -> &str {
        &self.login
    }
    pub async fn abort_tasks(&self) {
        let mut tasks = self.tasks.write().await;
        while let Some(task) = tasks.pop() {
            task.abort();
        }
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

#[derive(Debug)]
struct Subscription {
    subscribers: HashSet<ServerClient>,
    subtopics: HashMap<String, Subscription>,
    subtopics_any: Option<Box<Subscription>>, // +
    sub_any: HashSet<ServerClient>,           // #
}

impl Default for Subscription {
    fn default() -> Self {
        Self {
            subscribers: <_>::default(),
            subtopics: <_>::default(),
            subtopics_any: None,
            sub_any: <_>::default(),
        }
    }
}

impl Subscription {
    #[inline]
    fn is_empty(&self) -> bool {
        self.subscribers.is_empty()
            && self.subtopics.is_empty()
            && self.subtopics_any.is_none()
            && self.sub_any.is_empty()
    }
}

#[derive(Debug, Serialize)]
pub struct ServerClientDBStats {
    subscription_count: usize,
    client_count: usize,
}

#[derive(Debug)]
pub struct ServerClientDB {
    subscriptions: Subscription,
    client_topics: HashMap<ServerClient, HashSet<String>>,
    clients_by_token: HashMap<Token, ServerClient>,
    subscription_count: usize,
}

impl Default for ServerClientDB {
    fn default() -> Self {
        Self {
            subscriptions: <_>::default(),
            client_topics: <_>::default(),
            clients_by_token: <_>::default(),
            subscription_count: 0,
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
    pub async fn register_data_channel(
        &mut self,
        token: &Token,
        channel: async_channel::Sender<Arc<MessageFrame>>,
    ) -> Result<(async_channel::Sender<Arc<MessageFrame>>, ServerClient), Error> {
        if let Some(ref mut client) = self.clients_by_token.get_mut(token) {
            let mut dc = client.data_channel.write().await;
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
    pub fn get_stats(&self) -> ServerClientDBStats {
        ServerClientDBStats {
            subscription_count: self.subscription_count,
            client_count: self.clients_by_token.len(),
        }
    }
    pub fn register_client(&mut self, login: &str) -> ServerClient {
        trace!("registering new client");
        loop {
            let client = Arc::new(ServerClientData {
                token: Token::new(),
                login: login.to_owned(),
                data_channel: RwLock::new(None),
                tasks: RwLock::new(Vec::new()),
            });
            if !self.client_topics.contains_key(&client) {
                self.client_topics.insert(client.clone(), HashSet::new());
                self.clients_by_token
                    .insert(client.token.clone(), client.clone());
                trace!("client registered: {}", client);
                break client;
            }
        }
    }
    pub fn unregister_client(&mut self, client: &ServerClient) {
        trace!("unregistering: {}", client);
        let client_topics = self.client_topics.remove(client).expect(CLIENT_NOT_REG_ERR);
        for topic in client_topics {
            unsubscribe_rec(&mut self.subscriptions, topic.split('/'), client);
            self.subscription_count -= 1;
        }
        self.clients_by_token.remove(&client.token);
        trace!("client unregistered: {}", client);
    }
    /// # Errors
    ///
    /// Will return Err if the topic is invalid
    pub fn subscribe(&mut self, topic: &str, client: ServerClient) -> Result<(), Error> {
        trace!("subscribing {} to {}", client, topic);
        let client_topics = self
            .client_topics
            .get_mut(&client)
            .expect(CLIENT_NOT_REG_ERR);
        let t = prepare_topic(topic)?;
        if !client_topics.contains(&t) {
            subscribe_rec(&mut self.subscriptions, t.split('/'), &client);
            client_topics.insert(t);
            self.subscription_count += 1;
        }
        trace!("client subscribed: {} to {}", client, topic);
        Ok(())
    }
    /// # Errors
    ///
    /// Will return Err if the topic is invalid
    pub fn unsubscribe(&mut self, topic: &str, client: ServerClient) -> Result<(), Error> {
        trace!("subscribing {} from {}", client, topic);
        let client_topics = self
            .client_topics
            .get_mut(&client)
            .expect(CLIENT_NOT_REG_ERR);
        let t = prepare_topic(topic)?;
        if client_topics.contains(&t) {
            unsubscribe_rec(&mut self.subscriptions, t.split('/'), &client);
            client_topics.remove(topic);
            self.subscription_count -= 1;
        }
        trace!("client unsubscribed: {} from {}", client, topic);
        Ok(())
    }
    pub fn get_subscribers(&self, topic: &str) -> HashSet<ServerClient> {
        trace!("getting subscribers for topic: {}", topic);
        let mut result = HashSet::new();
        get_subscribers_rec(&self.subscriptions, topic.split('/'), &mut result);
        for r in &result {
            trace!("subscriber for topic {}: {}", topic, r);
        }
        result
    }
}

fn subscribe_rec(subscription: &mut Subscription, mut sp: Split<char>, client: &ServerClient) {
    if let Some(topic) = sp.next() {
        match topic {
            "#" => {
                subscription.sub_any.insert(client.clone());
            }
            "+" => {
                if let Some(ref mut sub) = subscription.subtopics_any {
                    subscribe_rec(sub, sp, client);
                } else {
                    let mut sub = Subscription::default();
                    subscribe_rec(&mut sub, sp, client);
                    subscription.subtopics_any = Some(Box::new(sub));
                }
            }
            v => {
                if let Some(sub) = subscription.subtopics.get_mut(v) {
                    subscribe_rec(sub, sp, client);
                } else {
                    let mut sub = Subscription::default();
                    subscribe_rec(&mut sub, sp, client);
                    subscription.subtopics.insert(v.to_owned(), sub);
                }
            }
        }
    } else {
        subscription.subscribers.insert(client.clone());
    }
}

fn unsubscribe_rec(subscription: &mut Subscription, mut sp: Split<char>, client: &ServerClient) {
    if let Some(topic) = sp.next() {
        match topic {
            "#" => {
                subscription.sub_any.remove(client);
            }
            "+" => {
                if let Some(ref mut sub) = subscription.subtopics_any {
                    unsubscribe_rec(sub, sp, client);
                    if sub.is_empty() {
                        subscription.subtopics_any = None;
                    }
                }
            }
            v => {
                if let Some(sub) = subscription.subtopics.get_mut(v) {
                    unsubscribe_rec(sub, sp, client);
                    if sub.is_empty() {
                        subscription.subtopics.remove(v);
                    }
                }
            }
        }
    } else {
        subscription.subscribers.remove(client);
    }
}

fn get_subscribers_rec(
    subscription: &Subscription,
    mut sp: Split<char>,
    result: &mut HashSet<ServerClient>,
) {
    result.extend(subscription.sub_any.clone());
    if let Some(topic) = sp.next() {
        if let Some(sub) = subscription.subtopics.get(topic) {
            get_subscribers_rec(sub, sp.clone(), result);
        }
        if let Some(ref sub) = subscription.subtopics_any {
            get_subscribers_rec(sub, sp, result);
        }
    } else {
        result.extend(subscription.subscribers.clone());
    }
}

impl Token {
    /// # Panics
    ///
    /// Should not panic
    pub fn new() -> Self {
        Self(
            rand::thread_rng()
                .sample_iter(&rand::distributions::Uniform::new(0, 0xff))
                .take(32)
                .map(u8::from)
                .collect::<Vec<u8>>()
                .try_into()
                .unwrap(),
        )
    }
    pub fn from(buf: [u8; 32]) -> Self {
        Self(buf)
    }
}

impl Default for Token {
    fn default() -> Self {
        Self::new()
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

#[cfg(test)]
mod test {
    use super::ServerClientDB;
    #[test]
    fn test_sub() {
        let mut db = ServerClientDB::default();
        let client = db.register_client("test");
        super::set_max_topic_depth(10);
        db.subscribe("unit/tests/test1", client.clone()).unwrap();
        db.subscribe("unit/tests/test2", client.clone()).unwrap();
        db.subscribe("unit/tests/test3", client.clone()).unwrap();
        db.unregister_client(&client);
        let client2 = db.register_client("test2");
        db.subscribe("unit/+/test2", client2.clone()).unwrap();
        db.subscribe("unit/zzz/test2", client2.clone()).unwrap();
        db.unsubscribe("unit/zzz/test2", client2.clone()).unwrap();
        let client3 = db.register_client("test3");
        db.subscribe("unit/+/+/+", client3.clone()).unwrap();
        db.unsubscribe("unit/+/+/+", client3.clone()).unwrap();
        let client4 = db.register_client("test4");
        db.subscribe("/unit/#", client4.clone()).unwrap();
        let subs = db.get_subscribers("unit/tests/test2");
        assert_eq!(subs.len(), 2);
        assert_eq!(subs.contains(&client2), true);
        assert_eq!(subs.contains(&client4), true);
    }
}
