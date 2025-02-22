//! Adapters for using [`std::io`] traits and their async equivalents with Bulk
//! and Interrupt endpoints.
//!
//! These types wrap an [`Endpoint`](crate::Endpoint) and manage transfers to
//! provide a higher-level buffered API.
//!
//! ## Examples
//!
//! ### Request-response
//!
//! ```no_run
//! use std::{io::{Read, Write}, time::Duration};
//! use nusb::{self, MaybeFuture, transfer::{Bulk, In, Out}};
//! let device_info = nusb::list_devices().wait().unwrap()
//!     .find(|dev| dev.vendor_id() == 0xAAAA && dev.product_id() == 0xBBBB)
//!     .expect("device not connected");
//!
//! let device = device_info.open().wait().expect("failed to open device");
//! let interface = device.claim_interface(0).wait().expect("failed to claim interface");
//!
//! let mut tx = interface.endpoint::<Bulk, Out>(0x01).unwrap()
//!     .writer(256)
//!     .with_num_transfers(4);
//! let mut rx = interface.endpoint::<Bulk, In>(0x81).unwrap()
//!     .reader(256)
//!     .with_num_transfers(4)
//!     .with_read_timeout(Duration::from_secs(1));
//!
//! tx.write_all(&[0x01, 0x02, 0x03]).unwrap();
//! tx.flush_end().unwrap();
//!
//! let mut rx_pkt = rx.until_short_packet();
//! let mut v = Vec::new();
//! rx_pkt.read_to_end(&mut v).unwrap();
//! rx_pkt.consume_end().unwrap();
//! ```
mod read;
pub use read::*;

mod write;
pub use write::*;
