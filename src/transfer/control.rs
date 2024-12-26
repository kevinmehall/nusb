use super::{ResponseBuffer, TransferRequest};

/// Transfer direction
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Direction {
    /// Host to device
    Out = 0,

    /// Device to host
    In = 1,
}

/// Specification defining the request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ControlType {
    /// Request defined by the USB standard.
    Standard = 0,

    /// Request defined by the standard USB class specification.
    Class = 1,

    /// Non-standard request.
    Vendor = 2,
}

#[cfg(target_arch = "wasm32")]
impl From<ControlType> for web_sys::UsbRequestType {
    fn from(value: ControlType) -> Self {
        match value {
            ControlType::Standard => web_sys::UsbRequestType::Standard,
            ControlType::Class => web_sys::UsbRequestType::Class,
            ControlType::Vendor => web_sys::UsbRequestType::Vendor,
        }
    }
}

/// Entity targeted by the request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Recipient {
    /// Request made to device as a whole.
    Device = 0,

    /// Request made to specific interface.
    Interface = 1,

    /// Request made to specific endpoint.
    Endpoint = 2,

    /// Other request.
    Other = 3,
}

#[cfg(target_arch = "wasm32")]
impl From<Recipient> for web_sys::UsbRecipient {
    fn from(value: Recipient) -> Self {
        match value {
            Recipient::Device => web_sys::UsbRecipient::Device,
            Recipient::Interface => web_sys::UsbRecipient::Interface,
            Recipient::Endpoint => web_sys::UsbRecipient::Endpoint,
            Recipient::Other => web_sys::UsbRecipient::Other,
        }
    }
}

/// SETUP packet without direction or buffers
pub struct Control {
    /// Request type used for the `bmRequestType` field sent in the SETUP packet.
    #[doc(alias = "bmRequestType")]
    pub control_type: ControlType,

    /// Recipient used for the `bmRequestType` field sent in the SETUP packet.
    #[doc(alias = "bmRequestType")]
    pub recipient: Recipient,

    /// `bRequest` field sent in the SETUP packet.
    #[doc(alias = "bRequest")]
    pub request: u8,

    /// `wValue` field sent in the SETUP packet.
    #[doc(alias = "wValue")]
    pub value: u16,

    /// `wIndex` field sent in the SETUP packet.
    ///
    /// For [`Recipient::Interface`] this is the interface number. For [`Recipient::Endpoint`] this is the endpoint number.
    #[doc(alias = "wIndex")]
    pub index: u16,
}

impl Control {
    pub(crate) fn request_type(&self, direction: Direction) -> u8 {
        request_type(direction, self.control_type, self.recipient)
    }
}

/// SETUP packet and associated data to make an **OUT** request on a control endpoint.
pub struct ControlOut<'a> {
    /// Request type used for the `bmRequestType` field sent in the SETUP packet.
    #[doc(alias = "bmRequestType")]
    pub control_type: ControlType,

    /// Recipient used for the `bmRequestType` field sent in the SETUP packet.
    #[doc(alias = "bmRequestType")]
    pub recipient: Recipient,

    /// `bRequest` field sent in the SETUP packet.
    #[doc(alias = "bRequest")]
    pub request: u8,

    /// `wValue` field sent in the SETUP packet.
    #[doc(alias = "wValue")]
    pub value: u16,

    /// `wIndex` field sent in the SETUP packet.
    ///
    /// For [`Recipient::Interface`] this is the interface number. For [`Recipient::Endpoint`] this is the endpoint number.
    #[doc(alias = "wIndex")]
    pub index: u16,

    /// Data to be sent in the data stage.
    #[doc(alias = "wLength")]
    pub data: &'a [u8],
}

impl ControlOut<'_> {
    #[allow(unused)]
    pub(crate) fn setup_packet(&self) -> Result<[u8; SETUP_PACKET_SIZE], ()> {
        Ok(pack_setup(
            Direction::Out,
            self.control_type,
            self.recipient,
            self.request,
            self.value,
            self.index,
            self.data.len().try_into().map_err(|_| ())?,
        ))
    }

    #[allow(unused)]
    pub(crate) fn request_type(&self) -> u8 {
        request_type(Direction::Out, self.control_type, self.recipient)
    }
}

impl TransferRequest for ControlOut<'_> {
    type Response = ResponseBuffer;
}

/// SETUP packet to make an **IN** request on a control endpoint.
pub struct ControlIn {
    /// Request type used for the `bmRequestType` field sent in the SETUP packet.
    #[doc(alias = "bmRequestType")]
    pub control_type: ControlType,

    /// Recipient used for the `bmRequestType` field sent in the SETUP packet.
    #[doc(alias = "bmRequestType")]
    pub recipient: Recipient,

    /// `bRequest` field sent in the SETUP packet.
    #[doc(alias = "bRequest")]
    pub request: u8,

    /// `wValue` field sent in the SETUP packet.
    #[doc(alias = "wValue")]
    pub value: u16,

    /// `wIndex` field sent in the SETUP packet.
    ///
    /// For [`Recipient::Interface`] this is the interface number. For [`Recipient::Endpoint`] this is the endpoint number.
    #[doc(alias = "wIndex")]
    pub index: u16,

    /// Number of bytes to be read in the data stage.
    #[doc(alias = "wLength")]
    pub length: u16,
}

impl ControlIn {
    #[allow(unused)]
    pub(crate) fn setup_packet(&self) -> [u8; SETUP_PACKET_SIZE] {
        pack_setup(
            Direction::In,
            self.control_type,
            self.recipient,
            self.request,
            self.value,
            self.index,
            self.length,
        )
    }

    #[allow(unused)]
    pub(crate) fn request_type(&self) -> u8 {
        request_type(Direction::In, self.control_type, self.recipient)
    }
}

pub(crate) const SETUP_PACKET_SIZE: usize = 8;

fn pack_setup(
    direction: Direction,
    control_type: ControlType,
    recipient: Recipient,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
) -> [u8; SETUP_PACKET_SIZE] {
    let bmrequesttype = request_type(direction, control_type, recipient);

    [
        bmrequesttype,
        request,
        (value & 0xFF) as u8,
        (value >> 8) as u8,
        (index & 0xFF) as u8,
        (index >> 8) as u8,
        (length & 0xFF) as u8,
        (length >> 8) as u8,
    ]
}

fn request_type(direction: Direction, control_type: ControlType, recipient: Recipient) -> u8 {
    ((direction as u8) << 7) | ((control_type as u8) << 5) | (recipient as u8)
}

impl TransferRequest for ControlIn {
    type Response = Vec<u8>;
}
