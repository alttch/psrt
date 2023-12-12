use crate::Config;
use eva_sdk::prelude::*;
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

const AUTHOR: &str = "Bohemia Automation";
const DESCRIPTION: &str = "PSRT server";

struct Handlers {
    info: ServiceInfo,
}

#[async_trait::async_trait]
impl RpcHandlers for Handlers {
    async fn handle_call(&self, event: RpcEvent) -> RpcResult {
        let method = event.parse_method()?;
        let payload = event.payload();
        match method {
            "stats.counters" => {
                if payload.is_empty() {
                    let s = { crate::STATS_COUNTERS.read().clone() };
                    Ok(Some(pack(&s)?))
                } else {
                    Err(RpcError::params(None))
                }
            }
            "stats.clients" => {
                if payload.is_empty() {
                    let s = { crate::DB.read().get_stats() };
                    Ok(Some(pack(&s)?))
                } else {
                    Err(RpcError::params(None))
                }
            }
            #[cfg(feature = "cluster")]
            "stats.cluster" => {
                if payload.is_empty() {
                    let s = psrt::replication::status().await;
                    Ok(Some(pack(&s)?))
                } else {
                    Err(RpcError::params(None))
                }
            }
            _ => svc_handle_default_rpc(method, &self.info),
        }
    }
}

pub async fn main(mut initial: Initial) -> EResult<()> {
    eva_sdk::service::set_poc(Some(Duration::from_secs(0)));
    let config: Config = Config::deserialize(
        initial
            .take_config()
            .ok_or_else(|| Error::invalid_data("config not specified"))?,
    )?;
    let mut info = ServiceInfo::new(AUTHOR, psrt::VERSION, DESCRIPTION);
    info.add_method(ServiceMethod::new("stats.counters"));
    info.add_method(ServiceMethod::new("stats.clients"));
    #[cfg(feature = "cluster")]
    info.add_method(ServiceMethod::new("stats.cluster"));
    let rpc = initial.init_rpc(Handlers { info }).await?;
    let client = rpc.client().clone();
    svc_init_logs(&initial, client.clone())?;
    svc_start_signal_handlers();
    let cdir = Path::new(initial.eva_dir()).to_owned();
    let timeout = config
        .proto
        .timeout
        .map_or_else(|| initial.timeout(), Duration::from_secs_f64);
    let ac = crate::AdditinalConfigs::load(&config, &cdir, timeout).await;
    let workers = usize::try_from(initial.workers()).unwrap();
    let listeners = crate::Listeners::create(&config.proto, &cdir)
        .await
        .map_err(Error::failed)?;
    crate::load_acl(&config.auth, &cdir).await;
    crate::load_auth(&config.auth, &cdir).await;
    initial.drop_privileges()?;
    let server_fut = tokio::spawn(async move {
        crate::launch(config, ac, listeners, cdir, timeout, workers, false).await;
    });
    let server_watch_fut = tokio::spawn(async move {
        let _ = server_fut.await;
        eva_sdk::service::poc();
    });
    svc_mark_ready(&client).await?;
    info!("{} started ({})", DESCRIPTION, initial.id());
    svc_block(&rpc).await;
    svc_mark_terminating(&client).await?;
    server_watch_fut.abort();
    std::process::exit(0);
}
