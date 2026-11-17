use anyhow::{Context, Result};
use nerv_shared::config::NervConfig;
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tracing::info;
use url::Url;

const OLLAMA_INSTALL_URL: &str = "https://ollama.com/install.sh";
const OLLAMA_START_TIMEOUT_SECS: u64 = 60;

pub async fn ensure_ollama_ready(config: &NervConfig) -> Result<()> {
    if config.models.router.provider != "ollama" {
        return Ok(());
    }

    if !config.brain.auto_install_ollama {
        return Ok(());
    }

    let base_url = &config.models.router.base_url;
    if !is_local_ollama_url(base_url)? {
        return Ok(());
    }

    if ollama_api_reachable(base_url)? {
        return Ok(());
    }

    info!(
        "Ollama is not reachable at {}. Running the official installer/startup script...",
        base_url
    );
    run_ollama_installer().await?;
    wait_for_ollama(base_url).await?;
    Ok(())
}

fn is_local_ollama_url(base_url: &str) -> Result<bool> {
    let url = Url::parse(base_url).context("invalid Ollama base_url")?;
    Ok(matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    ))
}

fn ollama_api_reachable(base_url: &str) -> Result<bool> {
    let url = Url::parse(base_url).context("invalid Ollama base_url")?;
    let host = url.host_str().context("Ollama base_url is missing host")?;
    let port = url
        .port_or_known_default()
        .context("Ollama base_url is missing port")?;
    let address = format!("{host}:{port}");
    let timeout = Duration::from_secs(1);

    let mut addrs = address
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve Ollama address: {address}"))?;

    for addr in addrs.by_ref() {
        if TcpStream::connect_timeout(&addr, timeout).is_ok() {
            return Ok(true);
        }
    }

    Ok(false)
}

async fn run_ollama_installer() -> Result<()> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("curl -fsSL {OLLAMA_INSTALL_URL} | sh"))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("failed to launch Ollama installer")?;

    if !status.success() {
        anyhow::bail!("Ollama installer exited with status {status}");
    }

    Ok(())
}

async fn wait_for_ollama(base_url: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(OLLAMA_START_TIMEOUT_SECS);

    while tokio::time::Instant::now() < deadline {
        if ollama_api_reachable(base_url)? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    anyhow::bail!(
        "Ollama is still not reachable at {} after installation/startup",
        base_url
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_ollama_urls_are_detected() {
        assert!(is_local_ollama_url("http://localhost:11434").unwrap());
        assert!(is_local_ollama_url("http://127.0.0.1:11434").unwrap());
        assert!(is_local_ollama_url("http://[::1]:11434").unwrap());
        assert!(!is_local_ollama_url("http://192.168.1.10:11434").unwrap());
        assert!(!is_local_ollama_url("https://ollama.internal:11434").unwrap());
    }
}
