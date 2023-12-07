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
        svc_handle_default_rpc(event.parse_method()?, &self.info)
    }
}

pub async fn main(mut initial: Initial) -> EResult<()> {
    eva_sdk::service::set_poc(Some(Duration::from_secs(0)));
    let config: Config = Config::deserialize(
        initial
            .take_config()
            .ok_or_else(|| Error::invalid_data("config not specified"))?,
    )?;
    let info = ServiceInfo::new(AUTHOR, psrt::VERSION, DESCRIPTION);
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
    initial.drop_privileges()?;
    let workers = usize::try_from(initial.workers()).unwrap();
    let listeners = crate::Listeners::create(&config.proto, &cdir)
        .await
        .map_err(Error::failed)?;
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
