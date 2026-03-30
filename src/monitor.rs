//! WebSocket monitor server — broadcasts live strategy state to connected clients.
//!
//! The server binds to a configurable host:port and fans out JSON messages via
//! a `tokio::sync::broadcast` channel. Zero overhead when no clients are connected.

use tokio::net::TcpListener;
use tokio::sync::broadcast;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

pub use crate::types::config::MonitorConfig;

/// Full strategy state snapshot, broadcast on every tick.
#[derive(serde::Serialize, Clone)]
pub struct MonitorTick {
    pub timestamp_ms: u64,
    pub strategy_name: String,
    pub mode: String,
    pub symbol: String,
    pub bid_price: f64,
    pub ask_price: f64,
    pub balance: f64,
    pub trade_count: usize,
    /// Price lines to render on the chart (provided by the strategy).
    pub price_lines: Vec<crate::strategy::PriceLine>,
    /// Strategy-specific state for the info panel.
    pub strategy_state: serde_json::Value,
}

/// Discrete order fill event.
#[derive(serde::Serialize, Clone)]
pub struct MonitorFill {
    pub timestamp_ms: u64,
    pub strategy_name: String,
    pub symbol: String,
    pub side: String,
    pub price: f64,
    pub quantity: f64,
    pub fill_type: String,
    pub profit_pct: Option<f64>,
    pub profit_usd: Option<f64>,
    /// Which ExitOrder.id triggered this fill (for exit fills).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_id: Option<String>,
    /// Whether this was a partial fill.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_partial: bool,
    /// Whether this fill closed the position (net qty reached 0).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub position_closed: bool,
}

/// Shadow optimization summary, broadcast periodically.
#[derive(serde::Serialize, Clone)]
pub struct ShadowSummary {
    pub timestamp_ms: u64,
    pub strategy_name: String,
    pub symbol: String,
    pub window_secs: u64,
    pub results: Vec<ShadowTrialResult>,
}

/// Metrics for a single shadow variant.
#[derive(serde::Serialize, Clone)]
pub struct ShadowTrialResult {
    pub variant: String,
    pub trade_count: usize,
    pub pnl: f64,
    pub pnl_pct: f64,
    pub max_drawdown_pct: f64,
    pub score: f64,
    pub eligible: bool,
}

/// Tagged event envelope for JSON serialization.
#[derive(serde::Serialize, Clone)]
#[serde(tag = "type")]
pub enum MonitorEvent {
    Tick(MonitorTick),
    Fill(MonitorFill),
    Shadow(ShadowSummary),
}

/// Broadcasts monitor events to all connected WebSocket clients.
pub struct MonitorBroadcaster {
    tx: broadcast::Sender<String>,
}

impl MonitorBroadcaster {
    /// Start the WS server and return a broadcaster handle.
    pub async fn start(config: &MonitorConfig) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let addr = format!("{}:{}", config.host, config.port);
        let listener = TcpListener::bind(&addr).await?;
        let (tx, _) = broadcast::channel::<String>(64);
        let tx_clone = tx.clone();

        tokio::spawn(async move {
            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        log::warn!("[monitor] accept error: {}", e);
                        continue;
                    }
                };
                let mut rx = tx_clone.subscribe();
                log::info!("[monitor] client connected: {}", peer);

                tokio::spawn(async move {
                    let ws = match tokio_tungstenite::accept_async(stream).await {
                        Ok(ws) => ws,
                        Err(e) => {
                            log::warn!("[monitor] ws handshake error: {}", e);
                            return;
                        }
                    };
                    let (mut sink, mut stream) = ws.split();

                    loop {
                        tokio::select! {
                            msg = rx.recv() => {
                                match msg {
                                    Ok(text) => {
                                        if sink.send(Message::Text(text)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Lagged(n)) => {
                                        log::debug!("[monitor] client {} lagged {} messages", peer, n);
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                }
                            }
                            ws_msg = stream.next() => {
                                match ws_msg {
                                    Some(Ok(Message::Close(_))) | None => break,
                                    Some(Err(_)) => break,
                                    _ => {}
                                }
                            }
                        }
                    }
                    log::info!("[monitor] client disconnected: {}", peer);
                });
            }
        });

        Ok(Self { tx })
    }

    /// Returns `true` if at least one monitor client is connected.
    pub fn has_clients(&self) -> bool {
        self.tx.receiver_count() > 0
    }

    /// Broadcast an event to all connected clients.
    /// No-op if no clients are connected.
    pub fn broadcast(&self, event: &MonitorEvent) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        if let Ok(json) = serde_json::to_string(event) {
            let _ = self.tx.send(json);
        }
    }
}
