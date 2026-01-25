#[macro_use]
extern crate bma_benchmark;
#[macro_use]
extern crate prettytable;
use clap::Clap;
use log::info;
use num_format::{Locale, ToFormattedString};
use prettytable::Table;
use rand::Rng;
use std::collections::BTreeMap;
use std::sync::{Arc, atomic};
use std::time::{Duration, Instant};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::RwLock;

use psrt::DEFAULT_PRIORITY;
use psrt::client;

static ERR_TOPIC_NOT_SPECIFIED: &str = "Topic not specified";

#[cfg(not(feature = "std-alloc"))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Clap)]
#[clap(version = psrt::VERSION, author = psrt::AUTHOR)]
struct Opts {
    #[clap(name = "host:port")]
    path: String,
    #[clap(short = 'u')]
    user: Option<String>,
    #[clap(short = 'p')]
    password: Option<String>,
    #[clap(long = "top", about = "monitor the most used topics")]
    top: bool,
    #[clap(short = 't')]
    topic: Option<String>,
    #[clap(short = 'm', about = "==size to generate")]
    message: Option<String>,
    #[clap(long = "benchmark")]
    benchmark: bool,
    #[clap(long = "benchmark-iterations", default_value = "10000")]
    benchmark_iterations: u32,
    #[clap(long = "benchmark-workers", default_value = "4")]
    benchmark_workers: u32,
    #[clap(long = "benchmark-cluster-sub", about = "subscribe to another node")]
    benchmark_cluster_sub: Option<String>,
    #[clap(long = "timeout", default_value = "5")]
    timeout: f64,
    #[clap(long = "tls")]
    tls: bool,
    #[clap(long = "tls-ca")]
    tls_ca: Option<String>,
}

struct BenchmarkWorker {
    id: String,
    client: client::Client,
    r_client: Option<client::Client>,
    data_channel: Arc<RwLock<async_channel::Receiver<psrt::Message>>>,
}

impl BenchmarkWorker {
    fn new(
        id: String,
        client: client::Client,
        r_client: Option<client::Client>,
        data_channel: async_channel::Receiver<psrt::Message>,
    ) -> Self {
        Self {
            id,
            client,
            r_client,
            data_channel: Arc::new(RwLock::new(data_channel)),
        }
    }
    fn is_connected(&self) -> bool {
        self.r_client.as_ref().map_or_else(
            || self.client.is_connected(),
            |r| r.is_connected() && self.client.is_connected(),
        )
    }
    async fn bye(&self) -> Result<(), psrt::Error> {
        self.client.bye().await?;
        if let Some(ref r) = self.r_client {
            r.bye().await?;
        }
        Ok(())
    }
}

#[allow(clippy::cast_possible_truncation)]
async fn benchmark_message(
    name: &str,
    message: &[u8],
    workers: &[Arc<BenchmarkWorker>],
    iterations: u32,
    wait_read: bool,
) {
    let mut futures = Vec::new();
    let message = Arc::new(message.to_vec());
    staged_benchmark_start!(name);
    for wrk in workers {
        assert!(wrk.is_connected());
        let test_topic = format!("benchmark/{}/test/{}", wrk.id, name);
        let worker = wrk.clone();
        let test_msg = message.clone();
        if wait_read {
            if let Some(ref r_client) = wrk.r_client {
                r_client.subscribe(test_topic.clone()).await.unwrap();
            } else {
                wrk.client.subscribe(test_topic.clone()).await.unwrap();
            }
            let data_fut = tokio::spawn(async move {
                let channel = worker.data_channel.write().await;
                for _ in 0..iterations {
                    let msg = channel.recv().await.unwrap();
                    assert_eq!(msg.data(), *test_msg);
                }
            });
            futures.push(data_fut);
        }
        let worker = wrk.clone();
        let test_msg = message.clone();
        let fut = tokio::spawn(async move {
            for _ in 0..iterations {
                worker
                    .client
                    .publish(DEFAULT_PRIORITY, test_topic.clone(), (*test_msg).clone())
                    .await
                    .unwrap();
            }
        });
        futures.push(fut);
    }
    for f in futures {
        f.await.unwrap();
    }
    staged_benchmark_finish_current!(workers.len() as u32 * iterations);
    for wrk in workers {
        assert!(wrk.is_connected());
        let test_topic = format!("benchmark/{}/test/{}", wrk.id, name);
        wrk.client.unsubscribe(test_topic.clone()).await.unwrap();
    }
}

fn prepare_stat_table() -> Table {
    let mut table = Table::new();
    let format = prettytable::format::FormatBuilder::new()
        .column_separator(' ')
        .borders(' ')
        .separators(
            &[prettytable::format::LinePosition::Title],
            prettytable::format::LineSeparator::new('-', '-', '-', '-'),
        )
        .padding(0, 1)
        .build();
    table.set_format(format);
    let titlevec: Vec<prettytable::Cell> = ["topic", "count", "bytes"]
        .iter()
        .map(|v| prettytable::Cell::new(v).style_spec("Fb"))
        .collect();
    table.set_titles(prettytable::Row::new(titlevec));
    table
}

//#[derive(Eq)]
struct TopicStat {
    topic: String,
    count: u64,
    bytes: u128,
}

impl TopicStat {
    fn new(topic: &str) -> Self {
        Self {
            topic: topic.to_owned(),
            count: 0,
            bytes: 0,
        }
    }
    fn count(&mut self, size: usize) {
        self.bytes += size as u128;
        self.count += 1;
    }
}

//impl Ord for TopicStat {
//fn cmp(&self, other: &Self) -> core::cmp::Ordering {
//self.count.cmp(&other.count)
//}
//}

//impl PartialOrd for TopicStat {
//fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
//Some(self.cmp(other))
//}
//}

//impl PartialEq for TopicStat {
//fn eq(&self, other: &Self) -> bool {
//self.count == other.count
//}
//}

struct MessageTest {
    name: String,
    data: Vec<u8>,
    iterations: u32,
}

#[allow(clippy::cast_sign_loss)]
#[allow(clippy::cast_possible_truncation)]
impl MessageTest {
    fn new(name: &str, iterations: u32) -> Self {
        let mut data = Vec::new();
        let size = byte_unit::Byte::from_str(name).unwrap().get_bytes();
        for i in 0..size {
            data.push(i as u8);
        }
        Self {
            name: name.to_owned(),
            data,
            iterations,
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn benchmark(
    config: &client::Config,
    benchmark_workers: u32,
    iterations: u32,
    sub_node: Option<&String>,
) {
    let it_total = benchmark_workers * iterations;
    info!(
        "Benchmarking, {} workers, {} iterations per worker...",
        benchmark_workers, iterations
    );
    let mut rng = rand::rng();
    let mut workers = Vec::new();
    for i in 0..benchmark_workers {
        let mut client = client::Client::connect(config).await.unwrap();
        let (data_channel, r_client) = if let Some(p) = sub_node {
            let mut r_config = config.clone();
            r_config.update_path(p);
            let mut r_client = client::Client::connect(&r_config).await.unwrap();
            (r_client.take_data_channel().unwrap(), Some(r_client))
        } else {
            (client.take_data_channel().unwrap(), None)
        };
        assert!(client.is_connected());
        let bi: u32 = rng.random();
        workers.push(Arc::new(BenchmarkWorker::new(
            format!("{}/{}", bi, i),
            client,
            r_client,
            data_channel,
        )));
    }
    let mut futures = Vec::new();
    staged_benchmark_start!("subscribe");
    for wrk in &workers {
        assert!(wrk.is_connected());
        let worker = wrk.clone();
        let fut = tokio::spawn(async move {
            for i in 0..iterations {
                worker
                    .client
                    .subscribe(format!("benchmark/{}/{}", worker.id, i))
                    .await
                    .unwrap();
            }
        });
        futures.push(fut);
    }
    for f in futures {
        f.await.unwrap();
    }
    staged_benchmark_finish_current!(it_total);
    let message_tests = vec![
        MessageTest::new("10b", iterations),
        MessageTest::new("1kb", iterations),
        MessageTest::new("10kb", iterations),
        MessageTest::new("100kb", iterations / 10),
        MessageTest::new("1mb", iterations / 100),
    ];
    for test in message_tests {
        benchmark_message(
            &format!("pub-{}", test.name),
            &test.data,
            &workers,
            test.iterations,
            false,
        )
        .await;
        benchmark_message(
            &format!("pub-read-{}", test.name),
            &test.data,
            &workers,
            test.iterations,
            true,
        )
        .await;
    }
    let mut futures = Vec::new();
    staged_benchmark_start!("unsubscribe");
    for wrk in &workers {
        assert!(wrk.is_connected());
        let worker = wrk.clone();
        let fut = tokio::spawn(async move {
            for i in 0..iterations {
                worker
                    .client
                    .subscribe(format!("benchmark/{}/{}", worker.id, i))
                    .await
                    .unwrap();
            }
        });
        futures.push(fut);
    }
    for f in futures {
        f.await.unwrap();
    }
    staged_benchmark_finish_current!(it_total);
    for wrk in &workers {
        wrk.bye().await.unwrap();
        assert!(!wrk.is_connected());
    }
    staged_benchmark_print!();
}

#[inline]
fn parse_topics(topic: Option<&String>) -> Vec<String> {
    topic
        .expect(ERR_TOPIC_NOT_SPECIFIED)
        .split(',')
        .map(ToOwned::to_owned)
        .collect::<Vec<String>>()
}

#[tokio::main(worker_threads = 1)]
#[allow(clippy::too_many_lines)]
async fn main() {
    let opts = Opts::parse();
    env_logger::Builder::new()
        .target(env_logger::Target::Stdout)
        .filter_level(if opts.benchmark || opts.top {
            log::LevelFilter::Info
        } else {
            log::LevelFilter::Trace
        })
        .init();
    let queue_size = if opts.benchmark { 256_000 } else { 4_096 };
    let user = opts.user.unwrap_or_default();
    let password = opts.password.unwrap_or_default();
    let tls_ca = if let Some(cafile) = opts.tls_ca {
        Some(tokio::fs::read_to_string(cafile).await.unwrap())
    } else {
        None
    };
    let mut config = client::Config::new(&opts.path)
        .set_auth(&user, &password)
        .set_queue_size(queue_size)
        .set_timeout(Duration::from_secs_f64(opts.timeout))
        .set_socket_wait_timeout(Duration::from_secs_f64(opts.timeout))
        .set_tls(opts.tls)
        .set_tls_ca(tls_ca)
        .build();
    if opts.benchmark {
        benchmark(
            &config,
            opts.benchmark_workers,
            opts.benchmark_iterations,
            opts.benchmark_cluster_sub.as_ref(),
        )
        .await;
    } else if opts.top {
        static SORT_MODE: atomic::AtomicU8 = atomic::AtomicU8::new(0);
        macro_rules! cls {
            () => {
                print!("{esc}[2J{esc}[1;1H", esc = 27 as char);
            };
        }
        let mut client = client::Client::connect(&config).await.unwrap();
        let data_channel = client.take_data_channel().unwrap();
        let mut topic_stats: BTreeMap<String, TopicStat> = BTreeMap::new();
        client
            .subscribe_bulk(parse_topics(opts.topic.as_ref()))
            .await
            .unwrap();
        let client = Arc::new(client);
        tokio::spawn(async move {
            signal(SignalKind::interrupt()).unwrap().recv().await;
            client.bye().await.unwrap();
            print!("{}[2J", 27 as char);
            if let Ok(term) = std::env::var("TERM")
                && term.starts_with("screen")
            {
                for s in &["reset", "cnorm"] {
                    let _r = std::process::Command::new("tput").arg(s).spawn();
                }
            }
            std::process::exit(0);
        });
        let mut last_refresh: Option<Instant> = None;
        let show_step = Duration::from_secs(1);
        let mut table = prepare_stat_table();
        let getch = getch::Getch::new();
        std::thread::spawn(move || {
            loop {
                let ch = getch.getch().unwrap();
                if ch as char == 's' {
                    let s = SORT_MODE.load(atomic::Ordering::SeqCst);
                    SORT_MODE.store(s ^ 1, atomic::Ordering::SeqCst);
                }
            }
        });
        table.add_row(row![' ', ' ', ' ']);
        cls!();
        table.printstd();
        loop {
            let message = data_channel.recv().await.unwrap();
            let topic = message.topic();
            if let Some(stat) = topic_stats.get_mut(topic) {
                stat.count(message.data().len());
            } else {
                let mut stat = TopicStat::new(topic);
                stat.count(message.data().len());
                topic_stats.insert(topic.to_owned(), stat);
            }
            if let Some(last_refresh) = last_refresh
                && last_refresh.elapsed() < show_step
            {
                continue;
            }
            last_refresh = Some(Instant::now());
            let mut stats: Vec<&TopicStat> = topic_stats.values().collect();
            stats.sort_by(|a, b| {
                if SORT_MODE.load(atomic::Ordering::SeqCst) == 0 {
                    b.count.cmp(&a.count)
                } else {
                    b.bytes.cmp(&a.bytes)
                }
            });
            let (_, h) = term_size::dimensions().unwrap();
            stats.truncate(h - 4);
            let mut table = prepare_stat_table();
            for s in stats {
                let byte = byte_unit::Byte::from_bytes(s.bytes);
                table.add_row(row![
                    s.topic,
                    s.count.to_formatted_string(&Locale::en).replace(',', "_"),
                    byte.get_appropriate_unit(false)
                ]);
            }
            cls!();
            table.printstd();
        }
    } else {
        if opts.message.is_some() {
            config = config.disable_data_stream();
        }
        let mut client = client::Client::connect(&config).await.unwrap();
        if let Some(ref msg) = opts.message {
            let topic = opts.topic.expect(ERR_TOPIC_NOT_SPECIFIED);
            if let Some(message_size) = msg.strip_prefix("==") {
                let mut m = Vec::new();
                let size = byte_unit::Byte::from_str(message_size).unwrap().get_bytes();
                for i in 0..size {
                    #[allow(clippy::cast_possible_truncation)]
                    m.push(i as u8);
                }
                info!("msg.len = {}", m.len());
                client.publish(DEFAULT_PRIORITY, topic, m).await.unwrap();
            } else {
                client
                    .publish(DEFAULT_PRIORITY, topic, msg.as_bytes().to_vec())
                    .await
                    .unwrap();
            }
        } else {
            let data_channel = client.take_data_channel().unwrap();
            let topics = parse_topics(opts.topic.as_ref());
            info!("Listening to {}...", topics.join(", "));
            client.subscribe_bulk(topics).await.unwrap();
            tokio::spawn(async move {
                signal(SignalKind::interrupt()).unwrap().recv().await;
                info!("terminating");
                client.bye().await.unwrap();
                std::process::exit(0);
            });
            loop {
                let message = data_channel.recv().await.unwrap();
                println!(
                    "{}\n---\n\"{}\"",
                    message.topic(),
                    message
                        .data_as_str()
                        .map_or_else(|_| format!("{:x?}", message.data()), ToOwned::to_owned)
                );
            }
        }
        client.bye().await.unwrap();
    }
}
