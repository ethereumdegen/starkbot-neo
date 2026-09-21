use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    license_url: String,
    license_sha256: String,
    artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug, Deserialize)]
struct Artifact {
    target: String,
    url: String,
    sha256: String,
    size: u64,
    archive_binary: String,
}

pub async fn fetch(manifest_path: &Path, target: &str, destination: &Path) -> Result<()> {
    let manifest_text = tokio::fs::read_to_string(manifest_path)
        .await
        .with_context(|| format!("could not read {}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&manifest_text).context("invalid Codex manifest")?;
    let artifact = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.target == target)
        .cloned()
        .ok_or_else(|| anyhow!("Codex manifest has no artifact for {target}"))?;
    validate_artifact(&artifact)?;

    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("Codex destination has no parent"))?;
    tokio::fs::create_dir_all(parent).await?;
    let archive_path = parent.join(".codex.download.tar.gz");
    let staged_binary = parent.join(".codex.staged");
    remove_if_present(&archive_path).await?;
    remove_if_present(&staged_binary).await?;

    let result = fetch_and_stage(&artifact, &archive_path, &staged_binary).await;
    if let Err(error) = result {
        let _ = remove_if_present(&archive_path).await;
        let _ = remove_if_present(&staged_binary).await;
        return Err(error);
    }
    tokio::fs::rename(&staged_binary, destination).await?;
    remove_if_present(&archive_path).await?;

    let output = match tokio::process::Command::new(destination)
        .arg("--version")
        .output()
        .await
    {
        Ok(output) => output,
        Err(error) => {
            let _ = remove_if_present(destination).await;
            return Err(error).context("downloaded Codex binary did not launch");
        }
    };
    let version = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !version.contains(&manifest.version) {
        let _ = remove_if_present(destination).await;
        bail!(
            "downloaded Codex version check failed: expected {}, got {}",
            manifest.version,
            version.trim()
        );
    }
    if let Err(error) = fetch_license(&manifest, &parent.join("LICENSE.codex")).await {
        let _ = remove_if_present(destination).await;
        return Err(error);
    }
    Ok(())
}

async fn fetch_license(manifest: &Manifest, destination: &Path) -> Result<()> {
    if !manifest
        .license_url
        .starts_with("https://raw.githubusercontent.com/openai/codex/")
        || !valid_digest(&manifest.license_sha256)
    {
        bail!("Codex license source is invalid");
    }
    let bytes = reqwest::Client::builder()
        .user_agent("starkbot-neo-codex-fetcher")
        .build()?
        .get(&manifest.license_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    if bytes.len() > 64 * 1024 {
        bail!("Codex license exceeds 64 KiB");
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != manifest.license_sha256 {
        bail!(
            "Codex license checksum mismatch: expected {}, got {digest}",
            manifest.license_sha256
        );
    }
    let staged = destination.with_extension("staged");
    tokio::fs::write(&staged, &bytes).await?;
    tokio::fs::rename(staged, destination).await?;
    Ok(())
}

async fn fetch_and_stage(
    artifact: &Artifact,
    archive_path: &Path,
    staged_binary: &Path,
) -> Result<()> {
    let response = reqwest::Client::builder()
        .user_agent("starkbot-neo-codex-fetcher")
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?
        .get(&artifact.url)
        .send()
        .await?
        .error_for_status()?;
    if let Some(length) = response.content_length()
        && length != artifact.size
    {
        bail!(
            "Codex archive length mismatch: expected {}, got {length}",
            artifact.size
        );
    }

    let mut output = tokio::fs::File::create(archive_path).await?;
    let mut hasher = Sha256::new();
    let mut received = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        received = received
            .checked_add(u64::try_from(chunk.len())?)
            .ok_or_else(|| anyhow!("Codex archive length overflow"))?;
        if received > MAX_ARCHIVE_BYTES || received > artifact.size {
            bail!("Codex archive exceeded its declared size");
        }
        hasher.update(&chunk);
        output.write_all(&chunk).await?;
    }
    output.flush().await?;
    output.sync_all().await?;
    if received != artifact.size {
        bail!(
            "Codex archive length mismatch: expected {}, got {received}",
            artifact.size
        );
    }
    let digest = format!("{:x}", hasher.finalize());
    if digest != artifact.sha256 {
        bail!(
            "Codex archive checksum mismatch: expected {}, got {digest}",
            artifact.sha256
        );
    }

    let archive_path = archive_path.to_owned();
    let staged_binary = staged_binary.to_owned();
    let archive_binary = artifact.archive_binary.clone();
    tokio::task::spawn_blocking(move || {
        extract_binary(&archive_path, &staged_binary, &archive_binary)
    })
    .await
    .context("Codex extraction task failed")??;
    Ok(())
}

fn extract_binary(archive_path: &Path, destination: &Path, binary_name: &str) -> Result<()> {
    let archive = File::open(archive_path)?;
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
    let mut found = false;
    for entry in archive.entries()? {
        let entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?;
        if path.file_name().and_then(|name| name.to_str()) != Some(binary_name) {
            continue;
        }
        if found {
            bail!("Codex archive contains the binary more than once");
        }
        if entry.size() > MAX_ARCHIVE_BYTES {
            bail!("Codex binary exceeds the extraction limit");
        }
        let mut output = File::create(destination)?;
        std::io::copy(&mut entry.take(MAX_ARCHIVE_BYTES + 1), &mut output)?;
        output.flush()?;
        output.sync_all()?;
        let mut permissions = output.metadata()?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(destination, permissions)?;
        found = true;
    }
    if !found {
        bail!("Codex archive does not contain `{binary_name}`");
    }
    Ok(())
}

fn validate_artifact(artifact: &Artifact) -> Result<()> {
    if artifact.size == 0 || artifact.size > MAX_ARCHIVE_BYTES {
        bail!("Codex artifact has an invalid declared size");
    }
    if !valid_digest(&artifact.sha256) {
        bail!("Codex artifact has an invalid SHA-256 digest");
    }
    if !artifact
        .url
        .starts_with("https://github.com/openai/codex/releases/download/")
    {
        bail!("Codex artifact URL is not an official GitHub release URL");
    }
    if artifact.archive_binary.contains('/') || artifact.archive_binary.contains("..") {
        bail!("Codex archive binary name is unsafe");
    }
    Ok(())
}

async fn remove_if_present(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn host_target() -> Result<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Ok("aarch64-apple-darwin"),
        ("x86_64", "macos") => Ok("x86_64-apple-darwin"),
        (arch, os) => bail!("Codex helper is not pinned for {arch}-{os}"),
    }
}

pub fn default_manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("third_party")
        .join("codex")
        .join("manifest.toml")
}

pub fn default_destination(target: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("vendor")
        .join("codex")
        .join(target)
        .join("codex")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_official_artifacts() {
        let artifact = Artifact {
            target: "aarch64-apple-darwin".into(),
            url: "https://example.com/codex.tar.gz".into(),
            sha256: "a".repeat(64),
            size: 10,
            archive_binary: "codex".into(),
        };
        assert!(validate_artifact(&artifact).is_err());
    }
}
