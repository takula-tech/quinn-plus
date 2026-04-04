#![cfg_attr(not(fuzzing), warn(missing_docs))]
#![cfg_attr(test, allow(dead_code))]
#![warn(unreachable_pub)]
#![allow(clippy::cognitive_complexity)]
#![allow(clippy::too_many_arguments)]
#![warn(clippy::use_self)]
mod cid_queue;
pub mod coding;
mod constant_time;
mod range_set;
#[cfg(all(test, any(feature = "rustls-aws-lc-rs", feature = "rustls-ring")))]
mod tests;
pub mod transport_parameters;
mod varint;
#[cfg(feature = "bloom")]
mod bloom_token_log;
mod connection;
mod config;
pub mod crypto;
mod frame;
mod endpoint;
mod packet;
mod shared;
mod transport_error;
pub mod congestion;
mod cid_generator;
mod token;
mod token_memory_cache;
#[cfg(fuzzing)]
pub mod fuzzing {}
