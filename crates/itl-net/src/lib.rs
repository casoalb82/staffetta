//! Network side of Ita Link: one task per network keeps the service connection
//! and a connection per puppet, translating IRC lines into core events and core
//! actions into IRC lines.

pub mod conn;
mod network;
pub mod pool;
mod puppet;

pub use network::{run_network, Inbound, NetSpec, ToNet};
