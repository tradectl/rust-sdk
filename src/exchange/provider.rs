use std::collections::HashMap;
use async_trait::async_trait;
use crate::types::MarketType;
use crate::exchange::market_adapter::{ExchangeResult, MarketAdapter};

pub struct ProviderConfig {
    pub exchange: String,
    pub market_types: Vec<MarketType>,
    pub testnet: bool,
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
}

/// Factory for MarketAdapter instances. Each exchange implements this trait
/// with `create_adapter` producing exchange-specific adapters. The default
/// `init` and `stop` methods handle lifecycle for all configured market types.
#[async_trait]
pub trait Provider: Send + Sync {
    fn config(&self) -> &ProviderConfig;
    fn markets(&self) -> &HashMap<MarketType, Box<dyn MarketAdapter>>;
    fn markets_mut(&mut self) -> &mut HashMap<MarketType, Box<dyn MarketAdapter>>;

    /// Create an exchange-specific adapter for the given market type.
    async fn create_adapter(
        &self,
        market_type: MarketType,
    ) -> ExchangeResult<Box<dyn MarketAdapter>>;

    fn exchange(&self) -> &str {
        &self.config().exchange
    }

    /// Initialize all configured market types.
    async fn init(&mut self) -> ExchangeResult<()> {
        let market_types = self.config().market_types.clone();
        for mt in market_types {
            let mut adapter = self.create_adapter(mt).await?;
            adapter.init().await?;
            self.markets_mut().insert(mt, adapter);
        }
        Ok(())
    }

    /// Get the adapter for a specific market type.
    fn get_market(&self, market_type: MarketType) -> Option<&dyn MarketAdapter> {
        self.markets().get(&market_type).map(|a| a.as_ref())
    }

    /// Get a mutable reference to the adapter for a specific market type.
    fn get_market_mut(&mut self, market_type: MarketType) -> Option<&mut Box<dyn MarketAdapter>> {
        self.markets_mut().get_mut(&market_type)
    }

    /// List available market types.
    fn get_market_types(&self) -> Vec<MarketType> {
        self.markets().keys().copied().collect()
    }

    /// Stop all market adapters.
    async fn stop(&mut self) -> ExchangeResult<()> {
        let keys: Vec<_> = self.markets().keys().copied().collect();
        for mt in keys {
            if let Some(adapter) = self.markets_mut().get_mut(&mt) {
                adapter.stop().await?;
            }
        }
        self.markets_mut().clear();
        Ok(())
    }
}
