use dashmap::DashMap;
use ethers::types::{Address, U256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Venue {
    Curve,
    Balancer,
    UniswapV3,
    Maverick,
}

impl Venue {
    pub fn to_u8(&self) -> u8 {
        match self {
            Venue::Curve => 1,
            Venue::Balancer => 2,
            Venue::UniswapV3 => 3,
            Venue::Maverick => 4,
        }
    }
    
    pub fn all() -> Vec<Venue> {
        vec![Venue::Curve, Venue::Balancer, Venue::UniswapV3]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Quote {
    pub buy_amount: U256,   // LST received per ETH spent
    pub sell_amount: U256,  // ETH received per LST sold
    pub liquidity: U256,    // Available liquidity
    pub timestamp_ms: u64,
}

impl Default for Quote {
    fn default() -> Self {
        Quote {
            buy_amount: U256::zero(),
            sell_amount: U256::zero(),
            liquidity: U256::zero(),
            timestamp_ms: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct QuoteKey {
    pub token: Address,
    pub venue: Venue,
}

pub struct PriceCache {
    quotes: DashMap<QuoteKey, Quote>,
    update_count: AtomicU64,
    last_update_ms: AtomicU64,
}

impl PriceCache {
    pub fn new() -> Self {
        Self {
            quotes: DashMap::new(),
            update_count: AtomicU64::new(0),
            last_update_ms: AtomicU64::new(0),
        }
    }
    
    pub fn update(&self, token: Address, venue: Venue, quote: Quote) {
        let key = QuoteKey { token, venue };
        self.quotes.insert(key, quote);
        self.update_count.fetch_add(1, Ordering::Relaxed);
        self.last_update_ms.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            Ordering::Relaxed
        );
    }
    
    pub fn get(&self, token: Address, venue: Venue) -> Option<Quote> {
        let key = QuoteKey { token, venue };
        self.quotes.get(&key).map(|q| *q)
    }
    
    pub fn get_all_for_token(&self, token: Address) -> Vec<(Venue, Quote)> {
        let mut results = Vec::new();
        for venue in Venue::all() {
            if let Some(quote) = self.get(token, venue) {
                results.push((venue, quote));
            }
        }
        results
    }
    
    pub fn is_stale(&self, max_age_ms: u64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let last = self.last_update_ms.load(Ordering::Relaxed);
        now - last > max_age_ms
    }
    
    pub fn update_count(&self) -> u64 {
        self.update_count.load(Ordering::Relaxed)
    }
}

impl Default for PriceCache {
    fn default() -> Self {
        Self::new()
    }
}
