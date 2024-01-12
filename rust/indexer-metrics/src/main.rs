// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::Parser;
use indexer_metrics::{
    metrics::{
        HASURA_API_LATEST_TRANSACTION_TIMESTAMP, HASURA_API_LATEST_VERSION,
        HASURA_API_LATEST_VERSION_TIMESTAMP, INDEXER_PROCESSED_LATENCY, PFN_LEDGER_TIMESTAMP,
        PFN_LEDGER_VERSION, TASK_FAILURE_COUNT,
    },
    util::{deserialize_from_string, fetch_url_with_timeout},
};
use serde::{Deserialize, Serialize};
use server_framework::{RunnableConfig, ServerArgs};
use tokio::time::Duration;

const QUERY_TIMEOUT_MS: u64 = 500;
const MIN_TIME_QUERIES_MS: u64 = 500;
const MICROSECONDS_MULTIPLIER: f64 = 1_000_000.0;

#[derive(Debug, Deserialize, Serialize)]
struct FullnodeResponse {
    #[serde(deserialize_with = "deserialize_from_string")]
    ledger_version: u64,
    #[serde(deserialize_with = "deserialize_from_string")]
    ledger_timestamp: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct HasuraResponse {
    processor_status: Vec<ProcessorStatus>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProcessorStatus {
    processor: String,
    last_success_version: i64,
    #[serde(deserialize_with = "deserialize_from_string")]
    last_updated: chrono::NaiveDateTime,
    #[serde(deserialize_with = "deserialize_from_string")]
    last_transaction_timestamp: chrono::NaiveDateTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessorConfig {
    pub hasura_rest_api_endpoint: Option<String>,
    pub fullnode_rest_api_endpoint: Option<String>,
    pub chain_name: String,
}

#[async_trait::async_trait]
impl RunnableConfig for PostProcessorConfig {
    async fn run(&self) -> Result<()> {
        let mut tasks = vec![];
        let hasura_rest_api_endpoint = self.hasura_rest_api_endpoint.clone();
        let fullnode_rest_api_endpoint = self.fullnode_rest_api_endpoint.clone();
        let chain_name = self.chain_name.clone();

        // if let Some(hasura) = hasura_rest_api_endpoint {}
        if let Some(fullnode) = fullnode_rest_api_endpoint {
            tasks.push(tokio::spawn(start_fn_fetch(fullnode, chain_name.clone())));
        }

        if let Some(hasura) = hasura_rest_api_endpoint {
            tasks.push(tokio::spawn(start_hasura_fetch(hasura, chain_name)));
        }
        let _ = futures::future::join_all(tasks).await;
        unreachable!("All tasks should run forever");
    }

    fn get_server_name(&self) -> String {
        "idxbg".to_string()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: ServerArgs = ServerArgs::parse();
    args.run::<PostProcessorConfig>(tokio::runtime::Handle::current())
        .await
}

async fn start_hasura_fetch(url: String, chain_name: String) {
    loop {
        let result = fetch_url_with_timeout(&url, QUERY_TIMEOUT_MS).await;
        let time_now = tokio::time::Instant::now();

        // Handle the result
        match result {
            Ok(Ok(response)) => match response.json::<HasuraResponse>().await {
                Ok(resp) => {
                    tracing::info!(url = &url, response = ?resp, "Request succeeded");
                    for processor in resp.processor_status {
                        let processor_name = processor.processor;
                        HASURA_API_LATEST_VERSION
                            .with_label_values(&[&processor_name, &chain_name])
                            .set(processor.last_success_version);
                        HASURA_API_LATEST_VERSION_TIMESTAMP
                            .with_label_values(&[&processor_name, &chain_name])
                            .set(processor.last_updated.timestamp_millis() as f64 / 1_000_000.0);
                        HASURA_API_LATEST_TRANSACTION_TIMESTAMP
                            .with_label_values(&[&processor_name, &chain_name])
                            .set(
                                processor.last_transaction_timestamp.timestamp_millis() as f64
                                    / 1_000_000.0,
                            );
                        INDEXER_PROCESSED_LATENCY
                            .with_label_values(&[&processor_name, &chain_name])
                            .set(
                                processor.last_updated.timestamp_millis() as f64 / 1_000_000.0
                                    - processor.last_transaction_timestamp.timestamp_millis()
                                        as f64
                                        / 1_000_000.0,
                            );
                    }
                },
                Err(err) => {
                    tracing::error!(url = &url, error = ?err, "Parsing error");
                    TASK_FAILURE_COUNT
                        .with_label_values(&["hasura", &chain_name])
                        .inc();
                },
            },
            Ok(Err(err)) => {
                // Request encountered an error within the timeout
                tracing::error!(url = &url, error = ?err, "Request error");
                TASK_FAILURE_COUNT
                    .with_label_values(&["hasura", &chain_name])
                    .inc();
            },
            Err(_) => {
                // Request timed out
                tracing::error!(url = &url, "Request timed out");
                TASK_FAILURE_COUNT
                    .with_label_values(&["hasura", &chain_name])
                    .inc();
            },
        }
        let elapsed = time_now.elapsed().as_millis() as u64;
        // Sleep for a max of 500ms between queries
        if elapsed < MIN_TIME_QUERIES_MS {
            tokio::time::sleep(Duration::from_millis(MIN_TIME_QUERIES_MS - elapsed)).await;
        }
    }
}

async fn start_fn_fetch(url: String, chain_name: String) {
    loop {
        let result = fetch_url_with_timeout(&url, QUERY_TIMEOUT_MS).await;
        let time_now = tokio::time::Instant::now();

        // Handle the result
        match result {
            Ok(Ok(response)) => match response.json::<FullnodeResponse>().await {
                Ok(resp) => {
                    tracing::info!(url = &url, response = ?resp, "Request succeeded");
                    PFN_LEDGER_VERSION
                        .with_label_values(&[&chain_name])
                        .set(resp.ledger_version as i64);
                    PFN_LEDGER_TIMESTAMP
                        .with_label_values(&[&chain_name])
                        .set(resp.ledger_timestamp as f64 / MICROSECONDS_MULTIPLIER);
                },
                Err(err) => {
                    tracing::error!(url = &url, error = ?err, "Parsing error");
                    TASK_FAILURE_COUNT
                        .with_label_values(&["fullnode", &chain_name])
                        .inc();
                },
            },
            Ok(Err(err)) => {
                // Request encountered an error within the timeout
                tracing::error!(url = &url, error = ?err, "Request error");
                TASK_FAILURE_COUNT
                    .with_label_values(&["fullnode", &chain_name])
                    .inc();
            },
            Err(_) => {
                // Request timed out
                tracing::error!(url = &url, "Request timed out");
                TASK_FAILURE_COUNT
                    .with_label_values(&["fullnode", &chain_name])
                    .inc();
            },
        }
        let elapsed = time_now.elapsed().as_millis() as u64;
        // Sleep for a max of 500ms between queries
        if elapsed < MIN_TIME_QUERIES_MS {
            tokio::time::sleep(Duration::from_millis(MIN_TIME_QUERIES_MS - elapsed)).await;
        }
    }
}
