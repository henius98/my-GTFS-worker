use std::{env, error::Error, process::Command};

fn watch_git_path(manifest_dir: &str, path: &str) -> Result<(), Box<dyn Error>> {
  let output = Command::new("git").args(["rev-parse", "--path-format=absolute", "--git-path", path]).current_dir(manifest_dir).output()?;
  if output.status.success() {
    println!("cargo:rerun-if-changed={}", String::from_utf8(output.stdout)?.trim());
  }
  Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
  let manifest_dir = env::var("CARGO_MANIFEST_DIR")?;
  let output = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&manifest_dir).output()?;
  if !output.status.success() {
    return Err("git rev-parse HEAD failed".into());
  }
  let sha = String::from_utf8(output.stdout)?;
  let version = sha.get(..6).ok_or("Git commit SHA is shorter than six characters")?;
  if !version.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err("Git commit SHA is invalid".into());
  }
  println!("cargo:rustc-env=SCHEMA_CACHE_VERSION={version}");

  watch_git_path(&manifest_dir, "HEAD")?;
  watch_git_path(&manifest_dir, "packed-refs")?;
  let reference = Command::new("git").args(["symbolic-ref", "HEAD"]).current_dir(&manifest_dir).output()?;
  if reference.status.success() {
    watch_git_path(&manifest_dir, String::from_utf8(reference.stdout)?.trim())?;
  }
  Ok(())
}
