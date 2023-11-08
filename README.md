nusb
----

A new pure-Rust library for cross-platform low-level access to USB devices.

[Documentation](https://docs.rs/nusb)

### Compared to [rusb](https://docs.rs/rusb/latest/rusb/) and [libusb](https://libusb.info/)

* Pure Rust, no dependency on libusb or any other C library.
* Async-first, while not requiring an async runtime like `tokio` or
  `async-std`. Still easily supports blocking with
  `futures_lite::block_on`.
* No context object. You just open a device. There is a global event loop thread
  that is started when opening the first device.
* Thinner layer over OS APIs, with less internal state.

### :construction: Current status

* Linux:  Control, bulk and interrupt transfers work, minimally tested
* Windows:  Control, bulk and interrupt transfers work, minimally tested
* macOS : [Not yet implemented](https://github.com/kevinmehall/nusb/issues/3)

### License
MIT or Apache 2.0, at your option
