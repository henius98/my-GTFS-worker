use worker::{D1Database, Env, Response, Result};

pub fn for_provider(env: &Env, provider: &str) -> Result<D1Database> {
  env.d1(&binding_name(provider))
}

pub fn binding_name(provider: &str) -> String {
  format!("DB_{}", provider.to_uppercase().replace("-", "_"))
}

pub fn provider_not_found(provider: &str) -> Result<Response> {
  Response::error(format!("Provider '{}' not found or DB not bound", provider), 404)
}
