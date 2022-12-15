use crate::{market_event_handler::SDKMarketEvent, sdk_client::SDKClient};
use solana_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Signature};
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::sync::mpsc::Sender;
use tokio::task::{spawn, JoinHandle};

pub struct EventPoller {
    pub worker: JoinHandle<()>,
}

impl EventPoller {
    pub fn new(
        sdk: Arc<SDKClient>,
        event_sender: Sender<Vec<SDKMarketEvent>>,
        timeout_ms: u64,
    ) -> Self {
        let worker = spawn(async move {
            Self::run(event_sender, sdk.clone(), timeout_ms).await;
        });

        Self { worker }
    }

    pub fn new_with_default_timeout(
        sdk: Arc<SDKClient>,
        event_sender: Sender<Vec<SDKMarketEvent>>,
    ) -> Self {
        Self::new(sdk, event_sender, 1000)
    }

    pub async fn run(
        event_sender: Sender<Vec<SDKMarketEvent>>,
        sdk: Arc<SDKClient>,
        timeout_ms: u64,
    ) {
        let mut until = None;
        // TODO: keep some state of signatures that have already been processed
        // TODO: make sure events are processed in order
        loop {
            let config = match until {
                None => GetConfirmedSignaturesForAddress2Config {
                    before: None,
                    until: None,
                    limit: Some(1),
                    commitment: Some(CommitmentConfig::confirmed()),
                },
                Some(until) => GetConfirmedSignaturesForAddress2Config {
                    before: None,
                    until: Some(until),
                    limit: None,
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            };

            // This is not 100% robust, but it's good enough for now.
            // TODO: join futures and await
            for (i, signature) in sdk
                .client
                .get_signatures_for_address_with_config(&sdk.core.active_market_key, config)
                .unwrap_or_default()
                .iter()
                .map(|tx| Signature::from_str(&tx.signature).unwrap())
                .enumerate()
                .rev()
            {
                if i == 0 {
                    until = Some(signature);
                }
                // TODO: This currently blocks on every iteration, which is not ideal.
                //       We should be able to spin up chunks of requests and join.
                println!("Processing transaction: {}", signature);
                let events = sdk.parse_events_from_transaction(&signature).await.unwrap();
                if event_sender
                    .send(
                        events
                            .iter()
                            .map(|&e| SDKMarketEvent::PhoenixEvent { event: Box::new(e) })
                            .collect::<Vec<_>>(),
                    )
                    .await
                    .is_err()
                {
                    println!("Event sender disconnected, continuing");
                    continue;
                }
            }
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
        }
    }
}
