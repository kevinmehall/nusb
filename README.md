nusb
----

A new pure-Rust library for cross-platform low-level access to USB devices.

* [Documentation](https://docs.rs/nusb)
* [Changelog](https://github.com/kevinmehall/nusb/releases)
* [Issues](https://github.com/kevinmehall/nusb/issues)
* [Discussions](https://github.com/kevinmehall/nusb/discussions)

`nusb` supports Windows, macOS, and Linux, and provides both async and
blocking APIs for listing and watching USB devices, reading descriptor
details, opening and managing devices and interfaces, and performing
transfers on control, bulk, and interrupt endpoints.

### Compared to [rusb](https://docs.rs/rusb/latest/rusb/) and [libusb](https://libusb.info/)

* Pure Rust, no dependency on libusb or any other C library.
* Async-first, while not requiring an async runtime.
* No context object. You just open a device. There is a global event loop thread
  that is started when opening the first device.
* Thinner layer over OS APIs, with less internal state.

### License

MIT or Apache 2.0, at your option
