#[macro_use]
extern crate bma_benchmark;

use clap::Clap;
use log::info;
use rand::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, RwLock};

use psrt::client;
use psrt::DEFAULT_PRIORITY;

const ERR_TOPIC_NOT_SPECIFIED: &str = "Topic not specified";

#[derive(Clap)]
#[clap(version = psrt::VERSION, author = psrt::AUTHOR)]
struct Opts {
    #[clap(name = "host:port")]
    path: String,
    #[clap(short = 'u')]
    user: Option<String>,
    #[clap(short = 'p')]
    password: Option<String>,
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
    data_channel: Arc<RwLock<mpsc::Receiver<psrt::Message>>>,
}

impl BenchmarkWorker {
    fn new(
        id: String,
        client: client::Client,
        r_client: Option<client::Client>,
        data_channel: mpsc::Receiver<psrt::Message>,
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
                let mut channel = worker.data_channel.write().await;
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
    let mut rng = rand::thread_rng();
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
        let bi: u32 = rng.gen();
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

#[tokio::main(worker_threads = 1)]
async fn main() {
    let opts = Opts::parse();
    env_logger::Builder::new()
        .target(env_logger::Target::Stdout)
        .filter_level(if opts.benchmark {
            log::LevelFilter::Info
        } else {
            log::LevelFilter::Trace
        })
        .init();
    let queue_size = if opts.benchmark { 256_000 } else { 4_096 };
    let user = opts.user.unwrap_or_else(|| "".to_owned());
    let password = opts.password.unwrap_or_else(|| "".to_owned());
    let tls_ca = if let Some(cafile) = opts.tls_ca {
        Some(tokio::fs::read_to_string(cafile).await.unwrap())
    } else {
        None
    };
    let mut config = client::Config::new(&opts.path)
        .set_auth(&user, &password)
        .set_queue_size(queue_size)
        .set_timeout(Duration::from_secs_f64(opts.timeout))
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
    } else {
        if opts.message.is_some() {
            config = config.disable_data_stream();
        }
        let mut client = client::Client::connect(&config).await.unwrap();
        if let Some(ref msg) = opts.message {
            let topic = opts.topic.expect(ERR_TOPIC_NOT_SPECIFIED);
            if let Some(message_size) = msg.strip_prefix("==") {
                let mut m = Vec::new();
                let size = byte_unit::Byte::from_str(&message_size)
                    .unwrap()
                    .get_bytes();
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
            let mut data_channel = client.take_data_channel().unwrap();
            let topics = opts
                .topic
                .expect(ERR_TOPIC_NOT_SPECIFIED)
                .split(',')
                .into_iter()
                .map(ToOwned::to_owned)
                .collect::<Vec<String>>();
            info!("Listening to {}...", topics.join(", "));
            client.subscribe_bulk(topics).await.unwrap();
            tokio::spawn(async move {
                loop {
                    signal(SignalKind::interrupt()).unwrap().recv().await;
                    info!("terminating");
                    client.bye().await.unwrap();
                    std::process::exit(0);
                }
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
