use importer::config;
use importer::d1::D1Client;
use importer::feed::HttpFeedFetcher;
use importer::workflow::{self, ShutdownSignal};
use std::env;
use std::sync::Arc;

async fn wait_for_shutdown_signal() {
  #[cfg(unix)]
  {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
      Ok(mut sigterm) => {
        tokio::select! {
          _ = tokio::signal::ctrl_c() => {}
          _ = sigterm.recv() => {}
        }
      }
      Err(error) => {
        eprintln!("Failed to install SIGTERM handler: {error}");
        let _ = tokio::signal::ctrl_c().await;
      }
    }
  }
  #[cfg(not(unix))]
  {
    let _ = tokio::signal::ctrl_c().await;
  }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let account_id = env::var("CLOUDFLARE_ACCOUNT_ID").map_err(|_| "CLOUDFLARE_ACCOUNT_ID must be set")?;
  let api_token = env::var("CLOUDFLARE_API_TOKEN").map_err(|_| "CLOUDFLARE_API_TOKEN must be set")?;

  let providers_path = match env::var("PROVIDERS_CONFIG") {
    Ok(path) if !path.trim().is_empty() => path,
    Ok(_) => return Err("PROVIDERS_CONFIG must not be empty".into()),
    Err(env::VarError::NotPresent) => "providers.toml".to_owned(),
    Err(error) => return Err(format!("Invalid PROVIDERS_CONFIG: {error}").into()),
  };
  let providers = config::load_config(&providers_path)?;
  let runtime = Arc::new(config::RuntimeConfig::from_env()?);
  let d1_client = match env::var("D1_API_BASE_URL") {
    Ok(base_url) => D1Client::with_base_url(account_id, api_token, base_url, &runtime)?,
    Err(env::VarError::NotPresent) => D1Client::new(account_id, api_token, &runtime)?,
    Err(error) => return Err(format!("Invalid D1_API_BASE_URL: {error}").into()),
  };
  let feed_fetcher = Arc::new(HttpFeedFetcher::new(&runtime)?);

  let shutdown = ShutdownSignal::new();
  let signal_shutdown = shutdown.clone();
  let signal_task = tokio::spawn(async move {
    wait_for_shutdown_signal().await;
    println!("Shutdown signal received; draining in-flight batches and saving checkpoints");
    signal_shutdown.trigger();
  });

  let summary = workflow::run_import_with_shutdown(providers, runtime, d1_client, feed_fetcher, &providers_path, shutdown).await?;
  signal_task.abort();
  if summary.has_error {
    return Err("One or more providers failed to process".into());
  }

  Ok(())
}
