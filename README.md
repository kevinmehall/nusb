nusb
----

A new pure-Rust library for cross-platform low-level access to USB devices.

### Compared to [rusb](https://docs.rs/rusb/latest/rusb/) and libusb

* Pure Rust, no dependency on libusb or any other C library.
* Async-first, while not requiring an async runtime like `tokio` or
  `async-std`. Still easily supports blocking with
  `futures_lite::block_on`.
* No context object. You just open a device. There is a global event loop thread
  that is started when opening the first device.
* Doesn't try to paper over OS differences. For example, on Windows, you must open
  a specific interface, not a device as a whole. `nusb`'s API makes working with interfaces
  a required step so that it can map directly to Windows APIs.

### Current status

:construction: Control, bulk and interrupt transfers work on Linux, minimally tested

### License
MIT or Apache 2.0, at your option
