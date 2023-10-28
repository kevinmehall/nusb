#![allow(non_snake_case)]
//! USB descriptor structures.

use core::slice;
use std::{
    collections::HashMap,
    mem::{size_of, transmute},
};

pub use crate::transfer::{Direction, EndpointType};

pub(crate) const DESCRIPTOR_TYPE_DEVICE: u8 = 0x01;
pub(crate) const DESCRIPTOR_TYPE_CONFIGURATION: u8 = 0x02;
pub(crate) const DESCRIPTOR_TYPE_INTERFACE: u8 = 0x04;
pub(crate) const DESCRIPTOR_TYPE_ENDPOINT: u8 = 0x05;

/// All standard descriptors have these 2 fields in common
#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
struct DescriptorHeader {
    bLength: u8,
    bDescriptorType: u8,
}

impl DescriptorHeader {
    fn from_slice(slice: &[u8]) -> Result<Self, ParseError> {
        if slice.len() < size_of::<Self>() {
            return Err(ParseError);
        }

        let bytes: [u8; size_of::<Self>()] = slice[..size_of::<Self>()].try_into().unwrap();
        // safety: self is valid for all bit patterns.
        let res: Self = unsafe { transmute(bytes) };

        // avoid infinite loop when bLength = 0
        if (res.bLength as usize) < size_of::<Self>() {
            return Err(ParseError);
        }

        Ok(res)
    }
}

/// Descriptors provided by the USB device are malformed.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) struct ParseError;

/// safety: self must be valid for all bit patterns, must not have padding holes.
pub(crate) unsafe trait Descriptor: Sized {
    fn from_slice(slice: &[u8]) -> Result<Self, ParseError> {
        if !Self::valid_length(slice.len()) {
            return Err(ParseError);
        }
        assert!(slice.len() <= size_of::<Self>());

        // safety: self is valid for all bit patterns.
        let mut res: Self = unsafe { std::mem::zeroed() };
        // safety: pointer is valid and not out of bounds.
        let res_bytes = unsafe {
            slice::from_raw_parts_mut((&mut res) as *mut _ as *mut u8, size_of::<Self>())
        };
        res_bytes[..slice.len()].copy_from_slice(slice);

        res.to_native_endian();
        Ok(res)
    }

    fn valid_length(len: usize) -> bool {
        len == core::mem::size_of::<Self>()
    }

    fn to_native_endian(&mut self) {}
}

macro_rules! to_native_endian {
    ($x:expr) => {
        // naming is a bit confusing:
        // We want to convert from bus endian (little endian) to native endian.
        // This means on little-endian hosts we don't have to do anything, and
        // on big-endian hosts we have to byte-swap. `to_le()` does just that.
        $x = $x.to_le()
    };
}

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
pub(crate) struct DeviceDescriptor {
    pub(crate) bLength: u8,
    pub(crate) bDescriptorType: u8,
    pub(crate) bcdUSB: u16,
    pub(crate) bDeviceClass: u8,
    pub(crate) bDeviceSubClass: u8,
    pub(crate) bDeviceProtocol: u8,
    pub(crate) bMaxPacketSize0: u8,
    pub(crate) idVendor: u16,
    pub(crate) idProduct: u16,
    pub(crate) bcdDevice: u16,
    pub(crate) iManufacturer: u8,
    pub(crate) iProduct: u8,
    pub(crate) iSerialNumber: u8,
    pub(crate) bNumConfigurations: u8,
}

unsafe impl Descriptor for DeviceDescriptor {
    fn to_native_endian(&mut self) {
        to_native_endian!(self.bcdUSB);
        to_native_endian!(self.idVendor);
        to_native_endian!(self.idProduct);
        to_native_endian!(self.bcdDevice);
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
struct ConfigurationDescriptor {
    bLength: u8,
    bDescriptorType: u8,
    wTotalLength: u16,
    bNumInterfaces: u8,
    bConfigurationValue: u8,
    iConfiguration: u8,
    bmAttributes: u8,
    bMaxPower: u8,
}

unsafe impl Descriptor for ConfigurationDescriptor {
    fn to_native_endian(&mut self) {
        to_native_endian!(self.wTotalLength)
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
struct InterfaceDescriptor {
    bLength: u8,
    bDescriptorType: u8,
    bInterfaceNumber: u8,
    bAlternateSetting: u8,
    bNumEndpoints: u8,
    bInterfaceClass: u8,
    bInterfaceSubClass: u8,
    bInterfaceProtocol: u8,
    iInterface: u8,
}

unsafe impl Descriptor for InterfaceDescriptor {}

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
struct EndpointDescriptor {
    bLength: u8,
    bDescriptorType: u8,
    bEndpointAddress: u8,
    bmAttributes: u8,
    wMaxPacketSize: u16,
    bInterval: u8,
    bRefresh: u8,
    bSynchAddress: u8,
}

unsafe impl Descriptor for EndpointDescriptor {
    fn to_native_endian(&mut self) {
        to_native_endian!(self.wMaxPacketSize)
    }
    fn valid_length(len: usize) -> bool {
        // there's 2 versions of the endpoint descriptor, one containing
        // bRefresh, bSynchAddress
        len == 7 || len == 9
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
struct BosDescriptor {
    bLength: u8,
    bDescriptorType: u8,
    wTotalLength: u16,
    bNumDeviceCaps: u8,
}

unsafe impl Descriptor for BosDescriptor {
    fn to_native_endian(&mut self) {
        to_native_endian!(self.wTotalLength)
    }
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn peek(&self) -> Result<Option<(u8, &'a [u8])>, ParseError> {
        if self.data.is_empty() {
            return Ok(None);
        }
        let header = DescriptorHeader::from_slice(self.data)?;
        let len = header.bLength as usize;
        if self.data.len() < len {
            return Err(ParseError);
        }

        Ok(Some((header.bDescriptorType, &self.data[..len])))
    }

    fn next(&mut self) -> Result<Option<(u8, &'a [u8])>, ParseError> {
        let r = self.peek()?;
        if let Some((_, data)) = &r {
            self.data = &self.data[data.len()..];
        }
        Ok(r)
    }

    fn capture_while_ty(&mut self, mut f: impl FnMut(u8) -> bool) -> Result<Vec<u8>, ParseError> {
        let start = self.data;

        while let Some((ty, _)) = self.peek()? {
            if !f(ty) {
                break;
            }
            self.next()?;
        }

        Ok(start[..start.len() - self.data.len()].to_vec())
    }
}

pub(crate) fn parse_configurations(
    descriptors: &[u8],
) -> Result<impl Iterator<Item = Configuration>, ParseError> {
    let mut r = Reader::new(descriptors);

    r.capture_while_ty(|ty| ty != DESCRIPTOR_TYPE_CONFIGURATION)?;

    let mut res = vec![];
    while let Some((DESCRIPTOR_TYPE_CONFIGURATION, _)) = r.peek()? {
        let configuration = Configuration::parse(&mut r)?;
        res.push(configuration);
    }

    Ok(res.into_iter())
}

/// USB device configuration.
#[derive(Debug)]
pub struct Configuration {
    descriptor: ConfigurationDescriptor,
    interfaces: Vec<Interface>,
    extra: Vec<u8>,
}

impl Configuration {
    fn parse(r: &mut Reader) -> Result<Self, ParseError> {
        let (ty, data) = r.next()?.unwrap();
        assert_eq!(ty, DESCRIPTOR_TYPE_CONFIGURATION);
        let descriptor = ConfigurationDescriptor::from_slice(data)?;

        let extra = r.capture_while_ty(|ty| {
            !matches!(
                ty,
                DESCRIPTOR_TYPE_DEVICE
                    | DESCRIPTOR_TYPE_CONFIGURATION
                    | DESCRIPTOR_TYPE_INTERFACE
                    | DESCRIPTOR_TYPE_ENDPOINT
            )
        })?;

        let mut interfaces: HashMap<u8, Interface> = HashMap::new();
        while let Some((DESCRIPTOR_TYPE_INTERFACE, _)) = r.peek()? {
            let alt = InterfaceAlternateSetting::parse(r)?;
            let number = alt.descriptor.bInterfaceNumber;
            let iface = interfaces.entry(number).or_insert_with(|| Interface {
                number,
                alternate_settings: vec![],
            });
            iface.alternate_settings.push(alt);
        }

        Ok(Self {
            descriptor,
            interfaces: interfaces.into_values().collect(),
            extra,
        })
    }

    /// Returns the configuration number.
    pub fn number(&self) -> u8 {
        self.descriptor.bConfigurationValue
    }

    /// Returns the device’s maximum power consumption (in milliamps) in this configuration.
    pub fn max_power(&self) -> u16 {
        self.descriptor.bMaxPower as u16 * 2
    }

    /// Indicates if the device is self-powered in this configuration.
    pub fn self_powered(&self) -> bool {
        self.descriptor.bmAttributes & 0x40 != 0
    }

    /// Indicates if the device has remote wakeup capability in this configuration.
    pub fn remote_wakeup(&self) -> bool {
        self.descriptor.bmAttributes & 0x20 != 0
    }

    /// Returns the index of the string descriptor that describes the configuration.
    pub fn description_string_index(&self) -> Option<u8> {
        match self.descriptor.iConfiguration {
            0 => None,
            n => Some(n),
        }
    }

    /// Returns the number of interfaces for this configuration.
    pub fn num_interfaces(&self) -> u8 {
        self.descriptor.bNumInterfaces
    }

    /// Returns a collection of the configuration's interfaces.
    pub fn interfaces(&self) -> impl Iterator<Item = &Interface> {
        self.interfaces.iter()
    }

    /// Returns unparsed class-specific descriptors.
    pub fn extra_descriptors(&self) -> &[u8] {
        &self.extra
    }
}

/// USB device interface
#[derive(Debug)]
pub struct Interface {
    number: u8,
    alternate_settings: Vec<InterfaceAlternateSetting>,
}

impl Interface {
    /// Get the interface number.
    pub fn number(&self) -> u8 {
        self.number
    }

    /// Returns a collection of the interface's alternate settings.
    pub fn alternate_settings(&self) -> impl Iterator<Item = &InterfaceAlternateSetting> {
        self.alternate_settings.iter()
    }
}

/// USB device interface alternate setting
#[derive(Debug)]
pub struct InterfaceAlternateSetting {
    descriptor: InterfaceDescriptor,
    endpoints: Vec<Endpoint>,
    extra: Vec<u8>,
}

impl InterfaceAlternateSetting {
    fn parse(r: &mut Reader) -> Result<Self, ParseError> {
        let (ty, data) = r.next()?.unwrap();
        assert_eq!(ty, DESCRIPTOR_TYPE_INTERFACE);
        let descriptor = InterfaceDescriptor::from_slice(data)?;

        let extra = r.capture_while_ty(|ty| {
            !matches!(
                ty,
                DESCRIPTOR_TYPE_DEVICE
                    | DESCRIPTOR_TYPE_CONFIGURATION
                    | DESCRIPTOR_TYPE_INTERFACE
                    | DESCRIPTOR_TYPE_ENDPOINT
            )
        })?;

        let mut endpoints = Vec::new();
        while let Some((DESCRIPTOR_TYPE_ENDPOINT, _)) = r.peek()? {
            endpoints.push(Endpoint::parse(r)?);
        }

        Ok(Self {
            descriptor,
            endpoints,
            extra,
        })
    }

    /// Returns the interface's number.
    pub fn interface_number(&self) -> u8 {
        self.descriptor.bInterfaceNumber
    }

    /// Returns the alternate setting number.
    pub fn alternate_setting_number(&self) -> u8 {
        self.descriptor.bAlternateSetting
    }

    /// Returns the interface's class code.
    pub fn class_code(&self) -> u8 {
        self.descriptor.bInterfaceClass
    }

    /// Returns the interface's sub class code.
    pub fn sub_class_code(&self) -> u8 {
        self.descriptor.bInterfaceSubClass
    }

    /// Returns the interface's protocol code.
    pub fn protocol_code(&self) -> u8 {
        self.descriptor.bInterfaceProtocol
    }

    /// Returns the index of the string descriptor that describes the interface.
    pub fn description_string_index(&self) -> Option<u8> {
        match self.descriptor.iInterface {
            0 => None,
            n => Some(n),
        }
    }

    /// Returns the number of endpoints belonging to this interface.
    pub fn num_endpoints(&self) -> u8 {
        self.descriptor.bNumEndpoints
    }

    /// Returns an iterator over the interface's endpoint descriptors.
    pub fn endpoints(&self) -> impl Iterator<Item = &Endpoint> {
        self.endpoints.iter()
    }

    /// Returns unparsed class-specific descriptors.
    pub fn extra_descriptors(&self) -> &[u8] {
        &self.extra
    }
}

/// Isochronous endpoint synchronization type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSyncType {
    /// No synchronization.
    NoSync,
    /// Asynchronous synchronization.
    Asynchronous,
    /// Adaptive synchronization.
    Adaptive,
    /// Synchronous synchronization.
    Synchronous,
}

/// Endpoint usage type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointUsageType {
    /// Data.
    Data,
    /// Feedback.
    Feedback,
    /// Feedback, data.
    FeedbackData,
    /// Reserved, do not use.
    Reserved,
}

/// USB endpoint.
#[derive(Debug)]
pub struct Endpoint {
    descriptor: EndpointDescriptor,
    extra: Vec<u8>,
}

impl Endpoint {
    fn parse(r: &mut Reader) -> Result<Self, ParseError> {
        let (ty, data) = r.next()?.unwrap();
        assert_eq!(ty, DESCRIPTOR_TYPE_ENDPOINT);
        let descriptor = EndpointDescriptor::from_slice(data)?;

        let extra = r.capture_while_ty(|ty| {
            !matches!(
                ty,
                DESCRIPTOR_TYPE_DEVICE
                    | DESCRIPTOR_TYPE_CONFIGURATION
                    | DESCRIPTOR_TYPE_INTERFACE
                    | DESCRIPTOR_TYPE_ENDPOINT
            )
        })?;

        Ok(Self { descriptor, extra })
    }

    /// Returns the endpoint's address.
    ///
    /// The address is a single byte containing the number in the lower 7 bits,
    /// and the direction in the highest bit.
    pub fn address(&self) -> u8 {
        self.descriptor.bEndpointAddress
    }

    /// Returns the endpoint number.
    ///
    /// This is the endpoint address without the direction bit.
    pub fn number(&self) -> u8 {
        self.descriptor.bEndpointAddress & 0x7f
    }

    /// Returns the endpoint's direction.
    pub fn direction(&self) -> Direction {
        match self.descriptor.bEndpointAddress & 0x80 {
            0 => Direction::Out,
            _ => Direction::In,
        }
    }

    /// Returns the endpoint's transfer type.
    pub fn transfer_type(&self) -> EndpointType {
        match self.descriptor.bmAttributes & 0x03 {
            0 => EndpointType::Control,
            1 => EndpointType::Isochronous,
            2 => EndpointType::Bulk,
            3 => EndpointType::Interrupt,
            _ => unreachable!(),
        }
    }

    /// Returns the endpoint's synchronisation mode.
    ///
    /// The return value of this method is only valid for isochronous endpoints.
    pub fn sync_type(&self) -> EndpointSyncType {
        match (self.descriptor.bmAttributes & 0x0c) >> 2 {
            0 => EndpointSyncType::NoSync,
            1 => EndpointSyncType::Asynchronous,
            2 => EndpointSyncType::Adaptive,
            3 => EndpointSyncType::Synchronous,
            _ => unreachable!(),
        }
    }

    /// Returns the endpoint's usage type.
    ///
    /// The return value of this method is only valid for isochronous endpoints.
    pub fn usage_type(&self) -> EndpointUsageType {
        match (self.descriptor.bmAttributes & 0x30) >> 4 {
            0 => EndpointUsageType::Data,
            1 => EndpointUsageType::Feedback,
            2 => EndpointUsageType::FeedbackData,
            3 => EndpointUsageType::Reserved,
            _ => unreachable!(),
        }
    }

    /// Returns the endpoint's maximum packet size.
    pub fn max_packet_size(&self) -> u16 {
        self.descriptor.wMaxPacketSize
    }

    /// Returns the endpoint's polling interval.
    pub fn interval(&self) -> u8 {
        self.descriptor.bInterval
    }

    /// For audio devices only: return the rate at which synchronization feedback is provided.
    pub fn refresh(&self) -> u8 {
        self.descriptor.bRefresh
    }

    /// For audio devices only: return the address if the synch endpoint.
    pub fn synch_address(&self) -> u8 {
        self.descriptor.bSynchAddress
    }

    /// Returns unparsed class-specific descriptors/
    pub fn extra_descriptors(&self) -> &[u8] {
        &self.extra
    }
}
