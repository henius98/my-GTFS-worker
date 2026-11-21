use importer::config;
use importer::d1::D1Client;
use importer::feed::HttpFeedFetcher;
use importer::workflow;
use std::env;
use std::sync::Arc;

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

  let summary = workflow::run_import(providers, runtime, d1_client, feed_fetcher, &providers_path).await?;
  if summary.has_error {
    return Err("One or more providers failed to process".into());
  }

  Ok(())
}
