use ethers::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use dashmap::DashMap;
use tracing::{info, warn, error};

pub type WsClient = Provider<Ws>;
pub type SignedClient = SignerMiddleware<Provider<Ws>, LocalWallet>;

#[derive(Debug, Clone)]
pub struct RpcHealth {
    pub url: String,
    pub latency_ms: u64,
    pub success_rate: f64,
    pub last_check: Instant,
    pub is_healthy: bool,
    pub consecutive_failures: u32,
}

pub struct RpcLoadBalancer {
    endpoints: Vec<String>,
    health: DashMap<String, RpcHealth>,
    primary: RwLock<Option<Arc<WsClient>>>,
    clients: DashMap<String, Arc<WsClient>>,
    max_latency_ms: u64,
}

impl RpcLoadBalancer {
    pub async fn new(
        primary_url: &str,
        backup_urls: &[&str],
        max_latency_ms: u64,
    ) -> eyre::Result<Self> {
        let mut endpoints = vec![primary_url.to_string()];
        endpoints.extend(backup_urls.iter().map(|s| s.to_string()));
        
        let lb = Self {
            endpoints,
            health: DashMap::new(),
            primary: RwLock::new(None),
            clients: DashMap::new(),
            max_latency_ms,
        };
        
        // Initialize connections
        lb.initialize_connections().await?;
        
        Ok(lb)
    }
    
    async fn initialize_connections(&self) -> eyre::Result<()> {
        for url in &self.endpoints {
            match self.connect(url).await {
                Ok(client) => {
                    self.clients.insert(url.clone(), Arc::new(client));
                    self.health.insert(url.clone(), RpcHealth {
                        url: url.clone(),
                        latency_ms: 0,
                        success_rate: 1.0,
                        last_check: Instant::now(),
                        is_healthy: true,
                        consecutive_failures: 0,
                    });
                    info!("Connected to RPC: {}", url);
                }
                Err(e) => {
                    warn!("Failed to connect to {}: {:?}", url, e);
                    self.health.insert(url.clone(), RpcHealth {
                        url: url.clone(),
                        latency_ms: u64::MAX,
                        success_rate: 0.0,
                        last_check: Instant::now(),
                        is_healthy: false,
                        consecutive_failures: 1,
                    });
                }
            }
        }
        
        // Set primary to first healthy
        self.select_primary().await;
        
        Ok(())
    }
    
    async fn connect(&self, url: &str) -> eyre::Result<WsClient> {
        let ws = Ws::connect(url).await?;
        let provider = Provider::new(ws).interval(Duration::from_millis(100));
        Ok(provider)
    }
    
    async fn select_primary(&self) {
        let mut best_url: Option<String> = None;
        let mut best_latency = u64::MAX;
        
        for entry in self.health.iter() {
            if entry.is_healthy && entry.latency_ms < best_latency {
                best_latency = entry.latency_ms;
                best_url = Some(entry.url.clone());
            }
        }
        
        if let Some(url) = best_url {
            if let Some(client) = self.clients.get(&url) {
                let mut primary = self.primary.write().await;
                *primary = Some(client.clone());
                info!("Primary RPC set to: {} ({}ms)", url, best_latency);
            }
        }
    }
    
    pub async fn get_client(&self) -> Option<Arc<WsClient>> {
        // Fast path: return primary if healthy
        {
            let primary = self.primary.read().await;
            if let Some(client) = primary.as_ref() {
                return Some(client.clone());
            }
        }
        
        // Fallback: find any healthy client
        for entry in self.health.iter() {
            if entry.is_healthy {
                if let Some(client) = self.clients.get(&entry.url) {
                    return Some(client.clone());
                }
            }
        }
        
        None
    }
    
    pub async fn health_check(&self) {
        for url in &self.endpoints {
            let client = match self.clients.get(url) {
                Some(c) => c.clone(),
                None => {
                    // Try to reconnect
                    match self.connect(url).await {
                        Ok(c) => {
                            self.clients.insert(url.clone(), Arc::new(c));
                            self.clients.get(url).unwrap().clone()
                        }
                        Err(_) => continue,
                    }
                }
            };
            
            let start = Instant::now();
            match tokio::time::timeout(
                Duration::from_millis(self.max_latency_ms * 2),
                client.get_block_number()
            ).await {
                Ok(Ok(_block)) => {
                    let latency = start.elapsed().as_millis() as u64;
                    
                    if let Some(mut health) = self.health.get_mut(url) {
                        health.latency_ms = latency;
                        health.success_rate = health.success_rate * 0.9 + 0.1;
                        health.is_healthy = latency < self.max_latency_ms;
                        health.last_check = Instant::now();
                        health.consecutive_failures = 0;
                    }
                }
                _ => {
                    if let Some(mut health) = self.health.get_mut(url) {
                        health.success_rate = health.success_rate * 0.9;
                        health.consecutive_failures += 1;
                        health.is_healthy = health.consecutive_failures < 3;
                        health.last_check = Instant::now();
                    }
                    
                    warn!("Health check failed for: {}", url);
                }
            }
        }
        
        // Re-select primary based on new health data
        self.select_primary().await;
    }
    
    pub fn get_health_stats(&self) -> Vec<RpcHealth> {
        self.health.iter().map(|e| e.value().clone()).collect()
    }
}

// Signed client for transactions
pub struct SignedClientManager {
    wallet: LocalWallet,
    lb: Arc<RpcLoadBalancer>,
    chain_id: u64,
}

impl SignedClientManager {
    pub fn new(wallet: LocalWallet, lb: Arc<RpcLoadBalancer>, chain_id: u64) -> Self {
        Self { wallet, lb, chain_id }
    }
    
    pub async fn get_client(&self) -> Option<SignerMiddleware<Arc<WsClient>, LocalWallet>> {
        let provider = self.lb.get_client().await?;
        let wallet = self.wallet.clone().with_chain_id(self.chain_id);
        Some(SignerMiddleware::new(provider, wallet))
    }
}
