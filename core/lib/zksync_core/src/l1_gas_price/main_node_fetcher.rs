use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use tokio::sync::watch::Receiver;
use zksync_system_constants::{GAS_PER_PUBDATA_BYTE, L1_GAS_PER_PUBDATA_BYTE};
use zksync_types::fee_model::{BatchFeeModelInput, MainNodeFeeModelConfig, MainNodeFeeParams};
use zksync_web3_decl::{
    jsonrpsee::http_client::{HttpClient, HttpClientBuilder},
    namespaces::ZksNamespaceClient,
};

use super::L1GasPriceProvider;
use crate::fee_model::{compute_batch_fee_model_input, BatchFeeModelInputProvider};

const SLEEP_INTERVAL: Duration = Duration::from_secs(5);

/// This structure maintains the known L1 gas price by periodically querying
/// the main node.
/// It is required since the main node doesn't only observe the current L1 gas price,
/// but also applies adjustments to it in order to smooth out the spikes.
/// The same algorithm cannot be consistently replicated on the external node side,
/// since it relies on the configuration, which may change.
#[derive(Debug)]
pub struct MainNodeBatchFeeInputFetcher {
    client: HttpClient,
    fee_model_output: RwLock<MainNodeFeeParams>,
}

impl MainNodeBatchFeeInputFetcher {
    pub fn new(main_node_url: &str) -> Self {
        Self {
            client: Self::build_client(main_node_url),
            fee_model_output: RwLock::new(MainNodeFeeParams {
                l1_gas_price: 1_000_000_000,
                l1_pubdata_price: 17_000_000_000,
                config: Default::default(),
            }),
        }
    }

    fn build_client(main_node_url: &str) -> HttpClient {
        HttpClientBuilder::default()
            .build(main_node_url)
            .expect("Unable to create a main node client")
    }

    pub async fn run(self: Arc<Self>, stop_receiver: Receiver<bool>) -> anyhow::Result<()> {
        loop {
            if *stop_receiver.borrow() {
                tracing::info!("Stop signal received, MainNodeGasPriceFetcher is shutting down");
                break;
            }

            let main_node_fee_params = match self.client.get_main_node_fee_params().await {
                Ok(price) => price,
                Err(err) => {
                    tracing::warn!("Unable to get the gas price: {}", err);
                    // A delay to avoid spamming the main node with requests.
                    tokio::time::sleep(SLEEP_INTERVAL).await;
                    continue;
                }
            };

            *self.fee_model_output.write().unwrap() = main_node_fee_params;

            tokio::time::sleep(SLEEP_INTERVAL).await;
        }
        Ok(())
    }
}

impl BatchFeeModelInputProvider for MainNodeBatchFeeInputFetcher {
    fn get_fee_model_params(&self) -> MainNodeFeeParams {
        self.fee_model_output.read().unwrap().clone()
    }
}
