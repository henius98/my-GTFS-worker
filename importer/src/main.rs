mod config;
mod d1;
mod processor;

use crate::d1::D1Client;
use std::env;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let account_id = env::var("CLOUDFLARE_ACCOUNT_ID").map_err(|_| "CLOUDFLARE_ACCOUNT_ID must be set")?;
    let api_token = env::var("CLOUDFLARE_API_TOKEN").map_err(|_| "CLOUDFLARE_API_TOKEN must be set")?;

    let config = config::load_config("providers.toml")?;

    let d1_client = D1Client::new(account_id, api_token);
    let csv_concurrency_limit = env::var("CSV_CONCURRENCY_LIMIT").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(9);
    let csv_semaphore = Arc::new(Semaphore::new(csv_concurrency_limit));
    let d1_concurrency_limit = env::var("D1_CONCURRENCY_LIMIT").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(20);
    let d1_semaphore = Arc::new(Semaphore::new(d1_concurrency_limit));
    let mut handles = vec![];

    for provider in config.providers.into_iter() {
        if !provider.is_active || provider.database_id.is_empty() {
            println!("[{}] Skipping provider: database_id is empty or is_active false", provider.name);
            continue;
        }

        let client = d1_client.clone();
        let csv_sem = csv_semaphore.clone();
        let d1_sem = d1_semaphore.clone();
        let handle = tokio::spawn(async move {
            println!("[{}] Processing provider", provider.name);
            if let Err(e) = processor::process_provider(&client, &provider, csv_sem, d1_sem).await {
                println!("[{}] Error processing provider: {}", provider.name, e);
                return Err(format!("Provider {} failed: {}", provider.name, e));
            }
            Ok(())
        });
        handles.push(handle);
    }

    let mut has_error = false;
    for handle in handles {
        match handle.await {
            Ok(Err(_e)) => {
                has_error = true;
            }
            Err(e) => {
                println!("Task failed to join: {}", e);
                has_error = true;
            }
            _ => {}
        }
    }

    if has_error {
        return Err("One or more providers failed to process".into());
    }

    Ok(())
}
