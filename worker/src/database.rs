use worker::{D1Database, Env, Response, Result};

pub fn for_provider(env: &Env, provider: &str) -> Result<D1Database> {
  let binding_name = format!("DB_{}", provider.to_uppercase().replace("-", "_"));

  env.d1(&binding_name)
}

pub fn provider_not_found(provider: &str) -> Result<Response> {
  Response::error(format!("Provider '{}' not found or DB not bound", provider), 404)
}
