use super::{ResponseBuffer, TransferRequest};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Direction {
    /// Host to device
    Out = 0,

    /// Device to host
    In = 1,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ControlType {
    Standard = 0,
    Class = 1,
    Vendor = 2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Recipient {
    Device = 0,
    Interface = 1,
    Endpoint = 2,
    Other = 3,
}

pub struct ControlOut<'a> {
    #[doc(alias = "bmRequestType")]
    pub control_type: ControlType,

    #[doc(alias = "bmRequestType")]
    pub recipient: Recipient,

    #[doc(alias = "bRequest")]
    pub request: u8,

    #[doc(alias = "wValue")]
    pub value: u16,

    #[doc(alias = "wIndex")]
    pub index: u16,

    #[doc(alias = "wLength")]
    pub data: &'a [u8],
}

impl<'a> ControlOut<'a> {
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

    pub(crate) fn request_type(&self) -> u8 {
        request_type(Direction::Out, self.control_type, self.recipient)
    }
}

impl TransferRequest for ControlOut<'_> {
    type Response = ResponseBuffer;
}

pub struct ControlIn {
    #[doc(alias = "bmRequestType")]
    pub control_type: ControlType,

    #[doc(alias = "bmRequestType")]
    pub recipient: Recipient,

    #[doc(alias = "bRequest")]
    pub request: u8,

    #[doc(alias = "windex")]
    pub value: u16,

    #[doc(alias = "wIndex")]
    pub index: u16,

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
