//! Maps server-side transport settings onto the protocol's channel config.

use std::time::Duration;

use protocol::ChannelConfig;

#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Maximum datagram size, header included.
    pub mtu: usize,
    /// Packets allowed in flight.
    pub window: u16,
    pub keepalive: Duration,
    pub idle_timeout: Duration,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            mtu: 1200,
            window: 64,
            keepalive: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(60),
        }
    }
}

impl From<TransportConfig> for ChannelConfig {
    fn from(c: TransportConfig) -> Self {
        ChannelConfig {
            mtu: c.mtu,
            window: c.window,
            keepalive: c.keepalive,
            idle_timeout: c.idle_timeout,
            ..ChannelConfig::default()
        }
    }
}
