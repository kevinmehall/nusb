//! Utilities for parsing USB descriptors.
//!
//! Descriptors are blocks of data that describe the functionality of a USB device.

use std::{
    collections::BTreeMap,
    fmt::{Debug, Display},
    io::ErrorKind,
    iter,
    ops::Deref,
};

use log::warn;

use crate::{
    transfer::{Direction, EndpointType},
    Error,
};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) const DESCRIPTOR_LEN_DEVICE: u8 = 18;

/// https://www.beyondlogic.org/usbnutshell/usb5.shtml#ConfigurationDescriptors
pub(crate) const DESCRIPTOR_TYPE_CONFIGURATION: u8 = 0x02;
/// https://www.beyondlogic.org/usbnutshell/usb5.shtml#ConfigurationDescriptors
pub(crate) const DESCRIPTOR_LEN_CONFIGURATION: u8 = 9;

/// https://www.beyondlogic.org/usbnutshell/usb5.shtml#ConfigurationDescriptors
pub(crate) const DESCRIPTOR_TYPE_INTERFACE: u8 = 0x04;
/// https://www.beyondlogic.org/usbnutshell/usb5.shtml#ConfigurationDescriptors
pub(crate) const DESCRIPTOR_LEN_INTERFACE: u8 = 9;

/// https://www.beyondlogic.org/usbnutshell/usb5.shtml#ConfigurationDescriptors
pub(crate) const DESCRIPTOR_TYPE_ENDPOINT: u8 = 0x05;
/// https://www.beyondlogic.org/usbnutshell/usb5.shtml#ConfigurationDescriptors
pub(crate) const DESCRIPTOR_LEN_ENDPOINT: u8 = 7;

/// https://www.beyondlogic.org/usbnutshell/usb5.shtml#ConfigurationDescriptors
pub(crate) const DESCRIPTOR_TYPE_STRING: u8 = 0x03;

/// USB defined language IDs for string descriptors.
///
/// In practice, different language IDs are not used,
/// and device string descriptors are only provided
/// with [`language_id::US_ENGLISH`].
pub mod language_id {
    /// US English
    pub const US_ENGLISH: u16 = 0x0409;
}

/// A raw USB descriptor.
///
/// Wraps a byte slice to provide access to the bytes of a descriptor by implementing `Deref` to `[u8]`,
/// while also exposing the descriptor length and type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor<'a>(&'a [u8]);

impl Descriptor<'_> {
    /// Create a `Descriptor` from a buffer.
    ///
    /// Returns `None` if
    ///   * the slice length is not at least 2.
    ///   * the `bLength` field (first byte) is greater than the slice length.
    pub fn new(buf: &[u8]) -> Option<Descriptor> {
        if buf.len() >= 2 && buf.len() >= buf[0] as usize {
            Some(Descriptor(buf))
        } else {
            None
        }
    }

    /// Get the length field of the descriptor.
    #[doc(alias = "bLength")]
    pub fn descriptor_len(&self) -> usize {
        self.0[0] as usize
    }

    /// Get the type field of the descriptor.
    #[doc(alias = "bDescriptorType")]
    pub fn descriptor_type(&self) -> u8 {
        self.0[1]
    }
}

impl Deref for Descriptor<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.0
    }
}

/// An iterator over a sequence of USB descriptors.
#[derive(Clone)]
pub struct Descriptors<'a>(&'a [u8]);

impl<'a> Descriptors<'a> {
    /// Get the concatenated bytes of the remaining descriptors.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.0
    }

    fn split_first(&self) -> Option<(&'a [u8], &'a [u8])> {
        if self.0.len() < 2 {
            return None;
        }

        if self.0[0] < 2 {
            warn!(
                "descriptor with bLength {} can't point to next descriptor",
                self.0[0]
            );
            return None;
        }

        if self.0[0] as usize > self.0.len() {
            warn!(
                "descriptor with bLength {} exceeds remaining buffer length {}",
                self.0[0],
                self.0.len()
            );
            return None;
        }

        Some(self.0.split_at(self.0[0] as usize))
    }

    fn split_by_type(mut self, descriptor_type: u8, min_len: u8) -> impl Iterator<Item = &'a [u8]> {
        iter::from_fn(move || {
            loop {
                let (_, next) = self.split_first()?;

                if self.0[1] == descriptor_type {
                    if self.0[0] >= min_len {
                        break;
                    } else {
                        warn!("ignoring descriptor of type {} and length {} because the minimum length is {}", self.0[1], self.0[0], min_len);
                    }
                }

                self.0 = next;
            }

            let mut end = self.0[0] as usize;

            while self.0.len() >= end + 2
                && self.0[end] > 2
                && self.0[end + 1] != descriptor_type
                && self.0.len() >= end + self.0[end] as usize
            {
                end += self.0[end] as usize;
            }

            let (r, next) = self.0.split_at(end);
            self.0 = next;
            Some(r)
        })
    }
}

impl<'a> Iterator for Descriptors<'a> {
    type Item = Descriptor<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((cur, next)) = self.split_first() {
            self.0 = next;
            Some(Descriptor(cur))
        } else {
            None
        }
    }
}

macro_rules! descriptor_fields {
    (impl<'a> $tname:ident<'a> {
        $(
            $(#[$attr:meta])*
            $vis:vis fn $name:ident at $pos:literal -> $ty:ty;
        )*
    }) => {
        impl<'a> $tname<'a> {
            $(
                $(#[$attr])*
                #[inline]
                $vis fn $name(&self) -> $ty { <$ty>::from_le_bytes(self.0[$pos..$pos + std::mem::size_of::<$ty>()].try_into().unwrap()) }
            )*
        }
    }
}

pub(crate) fn validate_config_descriptor(buf: &[u8]) -> Option<usize> {
    if buf.len() < DESCRIPTOR_LEN_CONFIGURATION as usize {
        if buf.is_empty() {
            warn!(
                "config descriptor buffer is {} bytes, need {}",
                buf.len(),
                DESCRIPTOR_LEN_CONFIGURATION
            );
        }
        return None;
    }

    if buf[0] < DESCRIPTOR_LEN_CONFIGURATION {
        warn!(
            "invalid config descriptor bLength. expected {DESCRIPTOR_LEN_CONFIGURATION}, got {}",
            buf[0]
        );
        return None;
    }

    if buf[1] != DESCRIPTOR_TYPE_CONFIGURATION {
        warn!(
            "config bDescriptorType is {}, not a configuration descriptor",
            buf[0]
        );
        return None;
    }

    let total_len = u16::from_le_bytes(buf[2..4].try_into().unwrap()) as usize;
    if total_len < buf[0] as usize || total_len > buf.len() {
        warn!(
            "invalid config descriptor wTotalLen of {total_len} (buffer size is {bufsize})",
            bufsize = buf.len()
        );
        return None;
    }

    Some(total_len)
}

/// Information about a USB configuration with access to all associated interfaces, endpoints, and other descriptors.
#[derive(Clone)]
pub struct Configuration<'a>(&'a [u8]);

impl<'a> Configuration<'a> {
    /// Create a `Configuration` from a buffer containing a series of descriptors.
    ///
    /// You normally obtain a `Configuration` from a [`Device`][crate::Device], but this allows creating
    /// one from your own descriptor bytes for tests.
    ///
    /// ### Panics
    ///  * when the buffer is too short for a configuration descriptor
    ///  * when the bLength and wTotalLength fields are longer than the buffer
    ///  * when the first descriptor is not a configuration descriptor
    pub fn new(buf: &[u8]) -> Configuration {
        assert!(buf.len() >= DESCRIPTOR_LEN_CONFIGURATION as usize);
        assert!(buf[0] as usize >= DESCRIPTOR_LEN_CONFIGURATION as usize);
        assert!(buf[1] == DESCRIPTOR_TYPE_CONFIGURATION);
        assert!(buf.len() == u16::from_le_bytes(buf[2..4].try_into().unwrap()) as usize);
        Configuration(buf)
    }

    /// Get the configuration descriptor followed by all trailing interface and other descriptors.
    pub fn descriptors(&self) -> Descriptors<'a> {
        Descriptors(self.0)
    }

    /// Iterate all interfaces and alternate settings settings of this configuration.
    pub fn interface_alt_settings(&self) -> impl Iterator<Item = InterfaceAltSetting<'a>> {
        self.descriptors()
            .split_by_type(DESCRIPTOR_TYPE_INTERFACE, DESCRIPTOR_LEN_INTERFACE)
            .map(InterfaceAltSetting)
    }

    /// Iterate the interfaces of this configuration, grouping together alternate settings of the same interface.
    pub fn interfaces(&self) -> impl Iterator<Item = InterfaceGroup<'a>> {
        let mut interfaces = BTreeMap::new();

        for intf in self.interface_alt_settings() {
            interfaces
                .entry(intf.interface_number())
                .or_insert_with(Vec::new)
                .push(intf);
        }

        interfaces
            .into_iter()
            .map(|(intf_number, interfaces)| InterfaceGroup {
                intf_number,
                interfaces,
            })
    }
}

descriptor_fields! {
    impl<'a> Configuration<'a> {
        /// `bNumInterfaces` descriptor field: Number of interfaces.
        #[doc(alias = "bNumInterfaces")]
        pub fn num_interfaces at 4 -> u8;

        /// `bConfigurationValue` descriptor field: Identifier for the configuration.
        ///
        /// Pass this value to
        /// [`Device::set_configuration`][crate::Device::set_configuration] to
        /// select this configuration.
        #[doc(alias = "bConfigurationValue")]
        pub fn configuration_value at 5 -> u8;

        fn string_index_raw at 6 -> u8;

        /// `bmAttributes` descriptor field: Bitmap of configuration attributes.
        #[doc(alias = "bmAttributes")]
        pub fn attributes at 7 -> u8;

        /// `bMaxPower` descriptor field: Maximum power, in units of **2** milliamps.
        #[doc(alias = "bMaxPower")]
        pub fn max_power at 8 -> u8;
    }
}

impl Configuration<'_> {
    /// Index of the string descriptor describing this configuration.
    #[doc(alias = "iConfiguration")]
    pub fn string_index(&self) -> Option<u8> {
        Some(self.string_index_raw()).filter(|&i| i != 0)
    }
}

struct DebugEntries<F>(F);

impl<F, I> Debug for DebugEntries<F>
where
    F: Fn() -> I,
    I: Iterator,
    I::Item: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0()).finish()
    }
}

impl Debug for Configuration<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Configuration")
            .field("configuration_value", &self.configuration_value())
            .field("num_interfaces", &self.num_interfaces())
            .field("attributes", &self.attributes())
            .field("max_power", &self.max_power())
            .field("string_index", &self.string_index())
            .field(
                "interface_alt_settings",
                &DebugEntries(|| self.interface_alt_settings()),
            )
            .finish()
    }
}

/// Interface descriptors for alternate settings, grouped by the interface number.
#[derive(Clone)]
pub struct InterfaceGroup<'a> {
    intf_number: u8,
    interfaces: Vec<InterfaceAltSetting<'a>>,
}

impl<'a> InterfaceGroup<'a> {
    /// `bInterfaceNumber` descriptor field: Identifier for the interface.
    ///
    /// Pass this to [`Device::claim_interface`][crate::Device::claim_interface] to work with the interface.
    #[doc(alias = "bInterfaceNumber")]
    pub fn interface_number(&self) -> u8 {
        self.intf_number
    }

    /// Iterator over alternate settings of the interface.
    pub fn alt_settings(&self) -> impl Iterator<Item = InterfaceAltSetting> {
        self.interfaces.iter().cloned()
    }

    /// Get the descriptor for the first alt setting.
    ///
    /// There is guaranteed to be at least one alt setting or this would not have been found.
    pub fn first_alt_setting(&self) -> InterfaceAltSetting<'a> {
        self.interfaces[0].clone()
    }
}

/// Information about a USB interface alternate setting, with access to associated endpoints and other descriptors.
///
/// An interface descriptor represents a single alternate setting of
/// an interface. Multiple interface descriptors with the same [`interface_number`][Self::interface_number]
/// but different [`alternate_setting`][Self::alternate_setting] values represent different alternate settings.
#[derive(Clone)]
pub struct InterfaceAltSetting<'a>(&'a [u8]);

impl InterfaceAltSetting<'_> {
    /// Get the interface descriptor followed by all trailing endpoint and other
    /// descriptors up to the next interface descriptor.
    pub fn descriptors(&self) -> Descriptors {
        Descriptors(self.0)
    }

    /// Get the endpoints of this interface.
    pub fn endpoints(&self) -> impl Iterator<Item = Endpoint> {
        self.descriptors()
            .split_by_type(DESCRIPTOR_TYPE_ENDPOINT, DESCRIPTOR_LEN_ENDPOINT)
            .map(Endpoint)
    }
}

descriptor_fields! {
    impl<'a> InterfaceAltSetting<'a> {
        /// `bInterfaceNumber` descriptor field: Identifier for the interface.
        ///
        /// Pass this to [`Device::claim_interface`][crate::Device::claim_interface] to work with the interface.
        #[doc(alias="bInterfaceNumber")]
        pub fn interface_number at 2 -> u8;

        /// `bAlternateSetting` descriptor field: Identifier for this alternate setting.
        ///
        /// Pass this to [`Interface::set_alt_setting`][crate::Interface::set_alt_setting] to use this alternate setting.
        #[doc(alias="bAlternateSetting")]
        pub fn alternate_setting at 3 -> u8;

        /// `bNumEndpoints` descriptor field: Number of endpoints in this alternate setting.
        #[doc(alias="bNumEndpoints")]
        pub fn num_endpoints at 4 -> u8;

        /// `bInterfaceClass` descriptor field: Standard interface class.
        #[doc(alias="bInterfaceClass")]
        pub fn class at 5 -> u8;

        /// `bInterfaceSubClass` descriptor field: Standard interface subclass.
        #[doc(alias="bInterfaceSubClass")]
        pub fn subclass at 6 -> u8;

        /// `bInterfaceProtocol` descriptor field: Standard interface protocol.
        #[doc(alias="bInterfaceProtocol")]
        pub fn protocol at 7 -> u8;

        fn string_index_raw at 8 -> u8;
    }
}

impl InterfaceAltSetting<'_> {
    /// Index of the string descriptor describing this interface or alternate setting.
    #[doc(alias = "iInterface")]
    pub fn string_index(&self) -> Option<u8> {
        Some(self.string_index_raw()).filter(|&i| i != 0)
    }
}

impl Debug for InterfaceAltSetting<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InterfaceAltSetting")
            .field("interface_number", &self.interface_number())
            .field("alternate_setting", &self.alternate_setting())
            .field("num_endpoints", &self.num_endpoints())
            .field("class", &self.class())
            .field("subclass", &self.subclass())
            .field("protocol", &self.protocol())
            .field("string_index", &self.string_index())
            .field("endpoints", &DebugEntries(|| self.endpoints()))
            .finish()
    }
}

/// Information about a USB endpoint, with access to any associated descriptors.
pub struct Endpoint<'a>(&'a [u8]);

impl Endpoint<'_> {
    /// Get the endpoint descriptor followed by all trailing descriptors up to the next endpoint or interface descriptor.
    pub fn descriptors(&self) -> impl Iterator<Item = Descriptor> {
        Descriptors(self.0)
    }

    /// Get the endpoint's direction.
    pub fn direction(&self) -> Direction {
        match self.address() & 0x80 {
            0 => Direction::Out,
            _ => Direction::In,
        }
    }

    /// Get the endpoint's transfer type.
    pub fn transfer_type(&self) -> EndpointType {
        match self.attributes() & 0x03 {
            0 => EndpointType::Control,
            1 => EndpointType::Isochronous,
            2 => EndpointType::Bulk,
            3 => EndpointType::Interrupt,
            _ => unreachable!(),
        }
    }

    /// Get the maximum packet size in bytes.
    pub fn max_packet_size(&self) -> usize {
        (self.max_packet_size_raw() & ((1 << 11) - 1)) as usize
    }

    /// For isochronous endpoints at high speed, get the number of packets per microframe (1, 2, or 3).
    pub fn packets_per_microframe(&self) -> u8 {
        ((self.max_packet_size_raw() >> 11) & 0b11) as u8 + 1
    }
}

descriptor_fields! {
    impl<'a> Endpoint<'a> {
        /// Get the `bEndpointAddress` descriptor field: Endpoint address.
        #[doc(alias = "bEndpointAddress")]
        pub fn address at 2 -> u8;

        /// Get the raw value of the `bmAttributes` descriptor field.
        ///
        /// See [`transfer_type``][Self::transfer_type] for the transfer type field.
        #[doc(alias = "bmAttributes")]
        pub fn attributes at 3 -> u8;

        /// Get the raw value of the `wMaxPacketSize` descriptor field.
        ///
        /// See [`max_macket_size`][Self::max_packet_size] and [`packets_per_microframe`][Self::packets_per_microframe]
        /// for the parsed subfields.
        #[doc(alias = "wMaxPacketSize")]
        pub fn max_packet_size_raw at 4 -> u16;

        /// Get the `bInterval` field: Polling interval in frames or microframes.
        #[doc(alias = "bInterval")]
        pub fn interval at 6 -> u8;
    }
}

impl Debug for Endpoint<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("address", &format_args!("0x{:02X}", self.address()))
            .field("direction", &self.direction())
            .field("transfer_type", &self.transfer_type())
            .field("max_packet_size", &self.max_packet_size())
            .field("packets_per_microframe", &self.packets_per_microframe())
            .field("interval", &self.interval())
            .finish()
    }
}

/// Error from [`crate::Device::active_configuration`]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ActiveConfigurationError {
    pub(crate) configuration_value: u8,
}

impl Display for ActiveConfigurationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.configuration_value == 0 {
            write!(f, "device is not configured")
        } else {
            write!(
                f,
                "no descriptor found for active configuration {}",
                self.configuration_value
            )
        }
    }
}

impl std::error::Error for ActiveConfigurationError {}

impl From<ActiveConfigurationError> for Error {
    fn from(value: ActiveConfigurationError) -> Self {
        Error::new(ErrorKind::Other, value)
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Split a chain of concatenated configuration descriptors by `wTotalLength`
pub(crate) fn parse_concatenated_config_descriptors(mut buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    iter::from_fn(move || {
        let total_len = validate_config_descriptor(buf)?;
        let descriptors = &buf[..total_len];
        buf = &buf[total_len..];
        Some(descriptors)
    })
}

pub(crate) fn validate_string_descriptor(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] as usize == data.len() && data[1] == DESCRIPTOR_TYPE_STRING
}

pub(crate) fn decode_string_descriptor(data: &[u8]) -> Result<String, ()> {
    if !validate_string_descriptor(data) {
        return Err(());
    }

    Ok(char::decode_utf16(
        data[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes(c.try_into().unwrap())),
    )
    .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
    .collect::<String>())
}

/// Make public when fuzzing
#[cfg(fuzzing)]
pub fn fuzz_parse_concatenated_config_descriptors(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    parse_concatenated_config_descriptors(buf)
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod test_concatenated {
    use super::parse_concatenated_config_descriptors;

    #[test]
    fn test_empty() {
        assert_eq!(
            parse_concatenated_config_descriptors(&[]).collect::<Vec<&[u8]>>(),
            Vec::<&[u8]>::new()
        );
    }

    #[test]
    fn test_short() {
        assert_eq!(
            parse_concatenated_config_descriptors(&[0]).collect::<Vec<&[u8]>>(),
            Vec::<&[u8]>::new()
        );
    }

    #[test]
    fn test_invalid_total_len() {
        assert_eq!(
            parse_concatenated_config_descriptors(&[9, 2, 0, 0, 0, 0, 0, 0, 0])
                .collect::<Vec<&[u8]>>(),
            Vec::<&[u8]>::new()
        );
    }

    #[test]
    fn test_one_config() {
        assert_eq!(
            parse_concatenated_config_descriptors(&[9, 2, 9, 0, 0, 0, 0, 0, 0])
                .collect::<Vec<&[u8]>>(),
            vec![&[9, 2, 9, 0, 0, 0, 0, 0, 0]]
        );

        assert_eq!(
            parse_concatenated_config_descriptors(&[9, 2, 13, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0])
                .collect::<Vec<&[u8]>>(),
            vec![&[9, 2, 13, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0]]
        );
    }

    #[test]
    fn test_two_configs() {
        assert_eq!(
            parse_concatenated_config_descriptors(&[
                9, 2, 13, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 9, 2, 9, 0, 0, 0, 0, 0, 0
            ])
            .collect::<Vec<&[u8]>>(),
            vec![
                [9, 2, 13, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0].as_slice(),
                [9, 2, 9, 0, 0, 0, 0, 0, 0].as_slice()
            ]
        );
    }
}

#[test]
fn test_empty_config() {
    let c = Configuration(&[9, 2, 9, 0, 0, 1, 0, 0, 250]);
    assert_eq!(c.num_interfaces(), 0);
    assert_eq!(c.configuration_value(), 1);
    assert_eq!(c.string_index(), None);
    assert_eq!(c.interfaces().count(), 0);
}

#[test]
fn test_malformed() {
    let c = Configuration(&[9, 2, 0, 0, 0, 1, 0, 0, 2, 5, 250, 0, 0, 0]);
    assert!(c.interfaces().next().is_none());
}

#[test]
#[rustfmt::skip]
fn test_linux_root_hub() {
    let c = Configuration(&[
        0x09, 0x02, 0x19, 0x00, 0x01, 0x01, 0x00, 0xe0, 0x00,
        0x09, 0x04, 0x00, 0x00, 0x01, 0x09, 0x00, 0x00, 0x00,
        0x07, 0x05, 0x81, 0x03, 0x04, 0x00, 0x0c
    ]);
    assert_eq!(c.num_interfaces(), 1);
    assert_eq!(c.configuration_value(), 1);
    assert_eq!(c.max_power(), 0);
    assert_eq!(c.interfaces().count(), 1);

    let interface = c.interfaces().next().unwrap();
    assert_eq!(interface.interface_number(), 0);

    let mut alts = interface.alt_settings();

    let alt = alts.next().unwrap();
    assert_eq!(alt.interface_number(), 0);
    assert_eq!(alt.alternate_setting(), 0);
    assert_eq!(alt.num_endpoints(), 1);
    assert_eq!(alt.class(), 9);
    assert_eq!(alt.subclass(), 0);
    assert_eq!(alt.protocol(), 0);
    assert_eq!(alt.endpoints().count(), 1);

    let endpoint = alt.endpoints().next().unwrap();
    assert_eq!(endpoint.address(), 0x81);
    assert_eq!(endpoint.transfer_type(), EndpointType::Interrupt);
    assert_eq!(endpoint.max_packet_size(), 4);
    assert_eq!(endpoint.interval(), 12);

    assert!(alts.next().is_none());
}

#[test]
#[rustfmt::skip]
fn test_dell_webcam() {
    let c = Configuration(&[
        0x09, 0x02, 0xa3, 0x02, 0x02, 0x01, 0x00, 0x80, 0xfa,
        
        // unknown (skipped)
        0x28, 0xff, 0x42, 0x49, 0x53, 0x54, 0x00, 0x01, 0x06, 0x01, 0x10, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xd1, 0x10, 0xd0, 0x07, 0xd2, 0x11, 0xf4, 0x01,
        0xd3, 0x12, 0xf4, 0x01, 0xd4, 0x13, 0xf4, 0x01, 0xd5, 0x14, 0xd0, 0x07,
        0xd6, 0x15, 0xf4, 0x01,

        // interface association
        0x08, 0x0b, 0x00, 0x02, 0x0e, 0x03, 0x00, 0x05,

        // interface
        0x09, 0x04, 0x00, 0x00, 0x01, 0x0e, 0x01, 0x00, 0x05,

        // VideoControl
        0x0d, 0x24, 0x01, 0x00, 0x01, 0x67, 0x00, 0xc0, 0xe1, 0xe4, 0x00, 0x01, 0x01,

        // VideoControl
        0x09, 0x24, 0x03, 0x05, 0x01, 0x01, 0x00, 0x04, 0x00,
        
        // VideoControl
        0x1a, 0x24, 0x06, 0x03, 0x70, 0x33, 0xf0, 0x28, 0x11, 0x63, 0x2e, 0x4a,
        0xba, 0x2c, 0x68, 0x90, 0xeb, 0x33, 0x40, 0x16, 0x08, 0x01, 0x02, 0x01,
        0x9f, 0x00,

        // VideoControl
        0x1a, 0x24, 0x06, 0x04, 0xc3, 0x85, 0xb8, 0x0f, 0xc2, 0x68, 0x47, 0x45,
        0x90, 0xf7, 0x8f, 0x47, 0x57, 0x9d, 0x95, 0xfc, 0x08, 0x01, 0x03, 0x01,
        0x0f, 0x00,
        
        // VideoControl
        0x12, 0x24, 0x02, 0x01, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x03, 0x0e, 0x00, 0x20,
        
        // VideoControl
        0x0b, 0x24, 0x05, 0x02, 0x01, 0x00, 0x00, 0x02, 0x7f, 0x17, 0x00,
        
        // Endpoint
        0x07, 0x05, 0x83, 0x03, 0x10, 0x00, 0x06,
        
        // Class-specific endpoint info
        0x05, 0x25, 0x03, 0x80, 0x00,
        
        // Interface
        0x09, 0x04, 0x01, 0x00, 0x00, 0x0e, 0x02, 0x00, 0x00,
        
        // Video Streaming
        0x0f, 0x24, 0x01, 0x02, 0x85, 0x01, 0x81, 0x00, 0x05, 0x00, 0x00, 0x00,
        0x01, 0x00, 0x00,
        
        // Video streaming
        0x0b, 0x24, 0x06, 0x01, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        
        // Video streaming (12 more omitted)
        0x06, 0x24, 0x0d, 0x01, 0x01, 0x04,
        
        // Interface
        0x09, 0x04, 0x01, 0x01, 0x01, 0x0e, 0x02, 0x00, 0x00,
        
        // Endpoint
        0x07, 0x05, 0x81, 0x05, 0x80, 0x00, 0x01,
        
        // Interface
        0x09, 0x04, 0x01, 0x02, 0x01, 0x0e, 0x02, 0x00, 0x00,
        
        // Endpoint
        0x07, 0x05, 0x81, 0x05, 0x00, 0x01, 0x01,
        
        // Interface
        0x09, 0x04, 0x01, 0x03, 0x01, 0x0e, 0x02, 0x00, 0x00,
        
        // Endpoint
        0x07, 0x05, 0x81, 0x05, 0x20, 0x03, 0x01,
        
        // Interface
        0x09, 0x04, 0x01, 0x04, 0x01, 0x0e, 0x02, 0x00, 0x00,
        
        // Endpoint
        0x07, 0x05, 0x81, 0x05, 0x20, 0x0b, 0x01,
        
        // Interface
        0x09, 0x04, 0x01, 0x05, 0x01, 0x0e, 0x02, 0x00, 0x00,
        
        // Endpoint
        0x07, 0x05, 0x81, 0x05, 0x20, 0x13, 0x01,
        
        // Interface
        0x09, 0x04, 0x01, 0x06, 0x01, 0x0e, 0x02, 0x00, 0x00,
        
        // Endpoint
        0x07, 0x05, 0x81, 0x05, 0x00, 0x14, 0x01
    ]);

    assert_eq!(c.configuration_value(), 1);
    assert_eq!(c.num_interfaces(), 2);
    assert_eq!(c.max_power(), 250);

    let mut interfaces = c.interfaces();
    let interface = interfaces.next().unwrap();
    assert_eq!(interface.interface_number(), 0);
    let mut alts = interface.alt_settings();

    let alt = alts.next().unwrap();
    assert_eq!(alt.interface_number(), 0);
    assert_eq!(alt.alternate_setting(), 0);
    assert_eq!(alt.num_endpoints(), 1);
    assert_eq!(alt.class(), 14);
    assert_eq!(alt.subclass(), 1);
    assert_eq!(alt.protocol(), 0);

    let mut descriptors = alt.descriptors();
    assert_eq!(descriptors.next().unwrap().descriptor_type(), DESCRIPTOR_TYPE_INTERFACE);
    for _ in 0..6 {
        assert_eq!(descriptors.next().unwrap().descriptor_type(), 0x24);
    }
    assert_eq!(descriptors.next().unwrap().descriptor_type(), DESCRIPTOR_TYPE_ENDPOINT);
    assert_eq!(descriptors.next().unwrap().descriptor_type(), 0x25);
    assert!(descriptors.next().is_none());

    let mut endpoints = alt.endpoints();

    let endpoint = endpoints.next().unwrap();
    assert_eq!(endpoint.address(), 0x83);
    assert_eq!(endpoint.transfer_type(), EndpointType::Interrupt);
    assert_eq!(endpoint.max_packet_size(), 16);

    assert_eq!(endpoint.descriptors().nth(1).unwrap().descriptor_type(), 0x25);
    
    assert!(endpoints.next().is_none());
    assert!(alts.next().is_none());

    let interface = interfaces.next().unwrap();
    assert_eq!(interface.interface_number(), 1);
    let mut alts = interface.alt_settings();

    let alt = alts.next().unwrap();
    assert_eq!(alt.interface_number(), 1);
    assert_eq!(alt.alternate_setting(), 0);
    assert_eq!(alt.num_endpoints(), 0);
    assert_eq!(alt.class(), 14);
    assert_eq!(alt.subclass(), 2);
    assert_eq!(alt.protocol(), 0);
    let mut endpoints = alt.endpoints();
    assert!(endpoints.next().is_none());

    let alt = alts.next().unwrap();
    assert_eq!(alt.interface_number(), 1);
    assert_eq!(alt.alternate_setting(), 1);
    assert_eq!(alt.num_endpoints(), 1);
    assert_eq!(alt.class(), 14);
    assert_eq!(alt.subclass(), 2);
    assert_eq!(alt.protocol(), 0);
    let mut endpoints = alt.endpoints();

    let endpoint = endpoints.next().unwrap();
    assert_eq!(endpoint.address(), 0x81);
    assert_eq!(endpoint.transfer_type(), EndpointType::Isochronous);
    assert_eq!(endpoint.max_packet_size(), 128);

    assert!(endpoints.next().is_none());

    let alt = alts.next().unwrap();
    assert_eq!(alt.interface_number(), 1);
    assert_eq!(alt.alternate_setting(), 2);
    assert_eq!(alt.num_endpoints(), 1);
    assert_eq!(alt.class(), 14);
    assert_eq!(alt.subclass(), 2);
    assert_eq!(alt.protocol(), 0);
    let mut endpoints = alt.endpoints();

    let endpoint = endpoints.next().unwrap();
    assert_eq!(endpoint.address(), 0x81);
    assert_eq!(endpoint.transfer_type(), EndpointType::Isochronous);
    assert_eq!(endpoint.max_packet_size(), 256);
    assert_eq!(endpoint.packets_per_microframe(), 1);

    assert!(endpoints.next().is_none());

    let alt = alts.next().unwrap();
    assert_eq!(alt.interface_number(), 1);
    assert_eq!(alt.alternate_setting(), 3);
    assert_eq!(alt.num_endpoints(), 1);
    assert_eq!(alt.class(), 14);
    assert_eq!(alt.subclass(), 2);
    assert_eq!(alt.protocol(), 0);
    let mut endpoints = alt.endpoints();

    let endpoint = endpoints.next().unwrap();
    assert_eq!(endpoint.address(), 0x81);
    assert_eq!(endpoint.transfer_type(), EndpointType::Isochronous);
    assert_eq!(endpoint.max_packet_size(), 800);
    assert_eq!(endpoint.packets_per_microframe(), 1);

    assert!(endpoints.next().is_none());

    let alt = alts.next().unwrap();
    assert_eq!(alt.interface_number(), 1);
    assert_eq!(alt.alternate_setting(), 4);
    assert_eq!(alt.num_endpoints(), 1);
    assert_eq!(alt.class(), 14);
    assert_eq!(alt.subclass(), 2);
    assert_eq!(alt.protocol(), 0);
    let mut endpoints = alt.endpoints();

    let endpoint = endpoints.next().unwrap();
    assert_eq!(endpoint.address(), 0x81);
    assert_eq!(endpoint.transfer_type(), EndpointType::Isochronous);
    assert_eq!(endpoint.max_packet_size(), 800);
    assert_eq!(endpoint.packets_per_microframe(), 2);

    let alt = alts.next().unwrap();
    assert_eq!(alt.interface_number(), 1);
    assert_eq!(alt.alternate_setting(), 5);
    assert_eq!(alt.num_endpoints(), 1);
    assert_eq!(alt.class(), 14);
    assert_eq!(alt.subclass(), 2);
    assert_eq!(alt.protocol(), 0);
    let mut endpoints = alt.endpoints();

    let endpoint = endpoints.next().unwrap();
    assert_eq!(endpoint.address(), 0x81);
    assert_eq!(endpoint.transfer_type(), EndpointType::Isochronous);
    assert_eq!(endpoint.max_packet_size(), 800);
    assert_eq!(endpoint.packets_per_microframe(), 3);

    let alt = alts.next().unwrap();
    assert_eq!(alt.interface_number(), 1);
    assert_eq!(alt.alternate_setting(), 6);
    assert_eq!(alt.num_endpoints(), 1);
    assert_eq!(alt.class(), 14);
    assert_eq!(alt.subclass(), 2);
    assert_eq!(alt.protocol(), 0);
    let mut endpoints = alt.endpoints();

    let endpoint = endpoints.next().unwrap();
    assert_eq!(endpoint.address(), 0x81);
    assert_eq!(endpoint.transfer_type(), EndpointType::Isochronous);
    assert_eq!(endpoint.max_packet_size(), 1024);
    assert_eq!(endpoint.packets_per_microframe(), 3);

    assert!(endpoints.next().is_none());
    assert!(alts.next().is_none());
    assert!(interfaces.next().is_none());
}
