//! Reports trades to the tradectl platform API.
//!
//! Batches trades and sends them in the background so the trading loop isn't blocked.

use std::sync::mpsc;
use std::thread;

#[derive(Clone, serde::Serialize)]
pub struct TradeReport {
    pub symbol: String,
    pub side: String,
    pub quantity: String,
    pub price: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pnl: Option<String>,
    pub source: String,
    #[serde(rename = "executedAt")]
    pub executed_at: String,
}

pub struct TradeReporter {
    tx: mpsc::Sender<TradeReport>,
}

impl TradeReporter {
    /// Create a new reporter. Reads `TRADECTL_API_URL` and credentials from
    /// `~/.tradectl/credentials`. Returns `None` if credentials are missing.
    pub fn new(strategy_key: &str) -> Option<Self> {
        // Only report trades for starter/pro tiers
        if !check_tier_allowed() {
            log::debug!("trade reporter: skipping (free tier)");
            return None;
        }

        let api_url = std::env::var("TRADECTL_API_URL")
            .unwrap_or_else(|_| "https://tradectl.com".to_string());

        let creds_path = dirs_path();
        let creds_data = std::fs::read_to_string(&creds_path).ok()?;
        let creds: serde_json::Value = serde_json::from_str(&creds_data).ok()?;
        let api_key = creds.get("token")?.as_str()?.to_string();

        let strategy_key = strategy_key.to_string();
        let (tx, rx) = mpsc::channel::<TradeReport>();

        log::info!("trade reporter: reporting to {api_url} as {strategy_key}");

        thread::spawn(move || {
            let agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .new_agent();

            let mut batch: Vec<TradeReport> = Vec::new();
            let flush_interval = std::time::Duration::from_secs(5);

            loop {
                match rx.recv_timeout(flush_interval) {
                    Ok(trade) => {
                        batch.push(trade);
                        // Drain any additional pending trades
                        while let Ok(t) = rx.try_recv() {
                            batch.push(t);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        // Channel closed — flush remaining and exit
                        if !batch.is_empty() {
                            flush(&agent, &api_url, &api_key, &strategy_key, &mut batch);
                        }
                        return;
                    }
                }

                if !batch.is_empty() {
                    flush(&agent, &api_url, &api_key, &strategy_key, &mut batch);
                }
            }
        });

        Some(Self { tx })
    }

    pub fn report(&self, trade: TradeReport) {
        let _ = self.tx.send(trade);
    }
}

fn flush(
    agent: &ureq::Agent,
    api_url: &str,
    api_key: &str,
    strategy_key: &str,
    batch: &mut Vec<TradeReport>,
) {
    let body = serde_json::json!({
        "apiKey": api_key,
        "strategyKey": strategy_key,
        "trades": batch,
    });

    let url = format!("{api_url}/api/cli/trades");
    match agent.post(&url)
        .header("Content-Type", "application/json")
        .send_json(&body)
    {
        Ok(resp) => {
            let status = resp.status();
            if status == 200 || status == 201 {
                log::debug!("reported {} trades to platform", batch.len());
            } else {
                log::warn!("trade report failed (HTTP {})", status);
            }
        }
        Err(e) => {
            log::warn!("trade report failed: {e}");
        }
    }

    batch.clear();
}

fn dirs_path() -> String {
    if let Ok(home) = std::env::var("HOME") {
        format!("{home}/.tradectl/credentials")
    } else {
        "~/.tradectl/credentials".to_string()
    }
}

/// Check cached license JWT to see if tier is starter or pro.
/// Decodes the JWT payload without signature verification (local file).
fn check_tier_allowed() -> bool {
    let home = std::env::var("HOME").ok().unwrap_or_default();
    let path = format!("{home}/.tradectl/license.jwt");
    let jwt = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let jwt = jwt.trim();
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    // Decode payload (base64url)
    let payload = parts[1];
    let padded = match payload.len() % 4 {
        2 => format!("{payload}=="),
        3 => format!("{payload}="),
        _ => payload.to_string(),
    };
    let standard = padded.replace('-', "+").replace('_', "/");
    let decoded = match base64_decode(&standard) {
        Some(d) => d,
        None => return false,
    };
    let claims: serde_json::Value = match serde_json::from_slice(&decoded) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let tier = claims.get("tier").and_then(|t| t.as_str()).unwrap_or("free");
    matches!(tier, "starter" | "pro")
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    // Simple base64 decoder — no extra dependency needed
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let bytes: Vec<u8> = input.bytes().filter(|&b| b != b'=').collect();
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for b in bytes {
        let val = TABLE.iter().position(|&c| c == b)? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}
