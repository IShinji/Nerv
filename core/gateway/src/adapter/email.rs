//! Email adapter — IMAP polling for inbound mail, SMTP for replies
//! (PRD v2 2.4, EmailAdapter). Feature-gated behind `email`.
//!
//! Authorization mirrors the Telegram adapter and fails closed: only
//! `allowed_senders` (exact email addresses) reach the brain. An unknown
//! sender gets no reply at all — an error would confirm the address is live.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use nerv_shared::config::EmailChannelConfig;
use nerv_shared::message::{ChannelType, Message};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::IncomingMessage;

pub struct EmailAdapter {
    config: EmailChannelConfig,
    #[allow(dead_code)]
    project_root: PathBuf,
}

impl EmailAdapter {
    pub fn new(config: EmailChannelConfig, project_root: PathBuf) -> Self {
        Self {
            config,
            project_root,
        }
    }

    /// The poll cadence for the IMAP loop.
    fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.config.poll_interval_secs.max(10))
    }
}

/// Extract the plain-text body of a parsed mail.
fn body_text(mail: &mail_parser::Message<'_>) -> Option<String> {
    mail.body_text(0)
        .map(|part| part.to_string())
        .or_else(|| mail.body_html(0).map(|html| strip_html(&html)))
}

/// Collapse an HTML body to readable text (a rough tag-strip — most mail
/// clients send a text part anyway).
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The bare address of a `From` header (`Name <addr@x>` → `addr@x`).
pub fn bare_address(from: &str) -> String {
    let from = from.trim();
    match from.rfind('<') {
        Some(start) if from.ends_with('>') => from[start + 1..from.len() - 1].to_string(),
        _ => from.to_string(),
    }
}

#[async_trait::async_trait]
impl super::ChannelAdapter for EmailAdapter {
    async fn start(&self, gateway_tx: mpsc::Sender<IncomingMessage>) -> Result<()> {
        info!(
            "Email adapter started (polling {}@{} every {}s)",
            self.config.username, self.config.imap_host, self.config.poll_interval_secs
        );

        let config = self.config.clone();
        let interval = self.poll_interval();
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = poll_once(&config, &gateway_tx).await {
                error!("Email poll failed: {e}");
            }
        }
    }

    async fn stop(&self) -> Result<()> {
        Ok(())
    }

    /// Replies go through SMTP to the sender address.
    async fn send_to(&self, sender: &str, text: &str) {
        if let Err(e) = smtp_send(&self.config, sender, "Re: Nerv", text).await {
            error!("Failed to send email reply to {sender}: {e}");
        }
    }

    async fn broadcast(&self, text: &str) {
        // Broadcast to every allowed sender (heartbeat results).
        for sender in &self.config.allowed_senders {
            self.send_to(sender, text).await;
        }
    }

    fn channel_type(&self) -> ChannelType {
        ChannelType::Email
    }
}

/// The TLS IMAP session type (tokio + rustls, native root certificates).
type ImapSession = async_imap::Session<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// Open an IMAPS session: TCP → TLS (rustls, native roots) → LOGIN.
async fn imap_connect(config: &EmailChannelConfig) -> Result<ImapSession> {
    use tokio_rustls::rustls::pki_types::ServerName;

    let tcp = tokio::net::TcpStream::connect((config.imap_host.as_str(), config.imap_port)).await?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = tokio_rustls::TlsConnector::from(std::sync::Arc::new(tls_config));
    let server_name = ServerName::try_from(config.imap_host.clone())
        .map_err(|_| anyhow::anyhow!("invalid IMAP host: {}", config.imap_host))?;
    let tls_stream = tls.connect(server_name, tcp).await?;

    let client = async_imap::Client::new(tls_stream);
    let session = client
        .login(&config.username, &config.password)
        .await
        .map_err(|(e, _client)| anyhow::anyhow!("IMAP login failed: {e}"))?;
    Ok(session)
}

/// One IMAP poll cycle: fetch unseen messages, dispatch them, mark seen.
async fn poll_once(
    config: &EmailChannelConfig,
    gateway_tx: &mpsc::Sender<IncomingMessage>,
) -> Result<()> {
    use futures_util::StreamExt;

    let mut session = imap_connect(config).await?;

    let mailbox = session.select(&config.mailbox).await?;
    info!(
        "Email poll: mailbox {} has {} messages ({} unseen)",
        config.mailbox,
        mailbox.exists,
        mailbox.unseen.unwrap_or(0)
    );

    // Unseen messages since the last poll.
    let fetch_query = "UNSEEN";
    let ids = session.uid_search(fetch_query).await?;
    for uid in ids {
        // Collect the fetch results first: the uid_store below needs the
        // session back, and the fetch stream borrows it.
        let mut bodies: Vec<Vec<u8>> = Vec::new();
        {
            let mut fetch_stream = session
                .uid_fetch(uid.to_string(), "(BODY.PEEK[])")
                .await
                .map_err(|e| anyhow::anyhow!("uid_fetch failed: {e}"))?;
            while let Some(fetch) = fetch_stream.next().await {
                match fetch {
                    Ok(fetch) => {
                        if let Some(body) = fetch.body() {
                            bodies.push(body.to_vec());
                        }
                    }
                    Err(e) => error!("Failed to read fetched message: {e}"),
                }
            }
        }
        for body in bodies {
            let mail = mail_parser::MessageParser::default().parse(&body);
            let Some(mail) = mail else { continue };
            let from = mail
                .from()
                .and_then(|f| f.first())
                .map(|a| a.address().unwrap_or_default().to_string())
                .unwrap_or_default();
            let sender = bare_address(&from);
            if !config.is_allowed(&sender) {
                // Mark seen without replying, so it never re-fires.
                warn!("Rejected email from unauthorized sender {sender}");
                continue;
            }
            let Some(text) = body_text(&mail) else {
                continue;
            };
            let message = Message::new_text(ChannelType::Email, &sender, &text);
            let (reply_tx, _reply_rx) = mpsc::channel::<Message>(1);
            if gateway_tx
                .send(IncomingMessage {
                    message,
                    reply_tx,
                    progress_tx: None,
                })
                .await
                .is_err()
            {
                error!("Gateway channel closed");
                break;
            }
        }
        mark_seen(&mut session, uid).await;
    }

    session.logout().await?;
    Ok(())
}

async fn mark_seen(session: &mut ImapSession, uid: u32) {
    use futures_util::StreamExt;

    let query = "+FLAGS (\\Seen)";
    match session.uid_store(uid.to_string(), query).await {
        // The store response stream must be drained, or the untagged
        // responses corrupt the next command's reply.
        Ok(updates) => {
            let _ = updates.collect::<Vec<_>>().await;
        }
        Err(e) => warn!("Failed to mark uid {uid} seen: {e}"),
    }
}

/// Send one email over SMTP (STARTTLS on the submission port).
async fn smtp_send(config: &EmailChannelConfig, to: &str, subject: &str, body: &str) -> Result<()> {
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

    let email = Message::builder()
        .from(lettre::message::Mailbox::new(
            None,
            config
                .username
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid from address: {}", config.username))?,
        ))
        .to(lettre::message::Mailbox::new(
            None,
            to.parse()
                .map_err(|_| anyhow::anyhow!("invalid to address: {to}"))?,
        ))
        .subject(subject)
        .body(body.to_string())?;

    let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host)?;
    mailer
        .credentials(lettre::transport::smtp::authentication::Credentials::new(
            config.username.clone(),
            config.password.clone(),
        ))
        .build()
        .send(email)
        .await?;
    info!("Sent email reply to {to}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(allowed: &[&str]) -> EmailChannelConfig {
        EmailChannelConfig {
            allowed_senders: allowed.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn bare_address_strips_display_names() {
        assert_eq!(
            bare_address("Wei Chen <wei@example.com>"),
            "wei@example.com"
        );
        assert_eq!(bare_address("wei@example.com"), "wei@example.com");
        assert_eq!(bare_address("  wei@example.com  "), "wei@example.com");
    }

    #[test]
    fn authorization_is_case_insensitive_and_fails_closed() {
        let cfg = config(&["Owner@Example.com"]);
        assert!(cfg.is_allowed("owner@example.com"));
        assert!(cfg.is_allowed("Owner@Example.com"));
        assert!(!cfg.is_allowed("stranger@example.com"));
        assert!(!cfg.is_allowed(""));
        assert!(!config(&[]).is_allowed("owner@example.com"));
    }

    #[test]
    fn html_bodies_are_stripped_to_text() {
        assert_eq!(strip_html("<p>hello <b>world</b></p>"), "hello world");
    }

    #[test]
    fn poll_interval_has_a_floor() {
        let mut cfg = config(&[]);
        cfg.poll_interval_secs = 1;
        let adapter = EmailAdapter::new(cfg, PathBuf::from("."));
        assert_eq!(adapter.poll_interval(), Duration::from_secs(10));
    }
}
