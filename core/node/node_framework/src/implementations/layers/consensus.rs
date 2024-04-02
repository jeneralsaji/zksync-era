use std::sync::Arc;

use zksync_concurrency::ctx;
use zksync_core::{
    consensus::{self, MainNodeConfig},
    sync_layer::{ActionQueueSender, MainNodeClient, SyncState},
};
use zksync_dal::{ConnectionPool, Core};

use crate::{
    implementations::resources::{
        action_queue::ActionQueueSenderResource, main_node_client::MainNodeClientResource,
        pools::MasterPoolResource, sync_state::SyncStateResource,
    },
    service::{ServiceContext, StopReceiver},
    task::Task,
    wiring_layer::{WiringError, WiringLayer},
};

#[derive(Debug, Copy, Clone)]
pub enum Mode {
    Main,
    External,
}

#[derive(Debug)]
pub struct ConsensusLayer {
    pub mode: Mode,
    pub config: Option<consensus::Config>,
    pub secrets: Option<consensus::Secrets>,
}

#[async_trait::async_trait]
impl WiringLayer for ConsensusLayer {
    fn layer_name(&self) -> &'static str {
        "consensus_layer"
    }

    async fn wire(self: Box<Self>, mut context: ServiceContext<'_>) -> Result<(), WiringError> {
        let pool = context
            .get_resource::<MasterPoolResource>()
            .await?
            .get()
            .await?;

        match self.mode {
            Mode::Main => {
                let config = self.config.ok_or_else(|| {
                    WiringError::Configuration("Missing public consensus config".to_string())
                })?;
                let secrets = self.secrets.ok_or_else(|| {
                    WiringError::Configuration("Missing private consensus config".to_string())
                })?;

                let main_node_config = config.main_node(&secrets)?;

                let task = MainNodeConsensusTask {
                    config: main_node_config,
                    pool,
                };
                context.add_task(Box::new(task));
            }
            Mode::External => {
                let main_node_client = context.get_resource::<MainNodeClientResource>().await?.0;
                let sync_state = context.get_resource::<SyncStateResource>().await?.0;
                let action_queue_sender = context
                    .get_resource::<ActionQueueSenderResource>()
                    .await?
                    .0
                    .take()
                    .ok_or_else(|| {
                        WiringError::Configuration(
                            "Action queue sender is taken by another resource".to_string(),
                        )
                    })?;

                let config = match (self.config, self.secrets) {
                    (Some(cfg), Some(secrets)) => Some((cfg, secrets)),
                    (Some(_), None) => {
                        return Err(WiringError::Configuration(
                            "Consensus config is specified, but secrets are missing".to_string(),
                        ));
                    }
                    (None, _) => {
                        // Secrets may be unconditionally embedded in some environments, but they are unused
                        // unless a consensus config is provided.
                        None
                    }
                };

                let task = FetcherTask {
                    config,
                    pool,
                    main_node_client,
                    sync_state,
                    action_queue_sender,
                };
                context.add_task(Box::new(task));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct MainNodeConsensusTask {
    config: MainNodeConfig,
    pool: ConnectionPool<Core>,
}

#[async_trait::async_trait]
impl Task for MainNodeConsensusTask {
    fn name(&self) -> &'static str {
        "consensus"
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        let root_ctx = ctx::root();
        zksync_core::consensus::run_main_node(&root_ctx, self.config, self.pool, stop_receiver.0)
            .await
    }
}

#[derive(Debug)]
pub struct FetcherTask {
    config: Option<(consensus::Config, consensus::Secrets)>,
    pool: ConnectionPool<Core>,
    main_node_client: Arc<dyn MainNodeClient>,
    sync_state: SyncState,
    action_queue_sender: ActionQueueSender,
}

#[async_trait::async_trait]
impl Task for FetcherTask {
    fn name(&self) -> &'static str {
        "consensus_fetcher"
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        let root_ctx = ctx::root();
        zksync_core::consensus::run_fetcher(
            &root_ctx,
            self.config,
            self.pool,
            self.sync_state,
            self.main_node_client,
            self.action_queue_sender,
            stop_receiver.0,
        )
        .await
    }
}
