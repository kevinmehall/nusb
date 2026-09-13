//! Raw isochronous transfer example.
//!
//! This does not configure a device-specific protocol such as UVC. The device
//! must already be ready to transfer data at the selected alternate setting.

use std::{error::Error, time::Duration};

use nusb::{
    transfer::{In, IsoCompletion, IsoLayout, IsoPacketStatus, Isochronous, Out},
    MaybeFuture,
};

const QUEUE_DEPTH: usize = 4;

fn parse_u16(value: &str) -> Result<u16, Box<dyn Error>> {
    Ok(if let Some(hex) = value.strip_prefix("0x") {
        u16::from_str_radix(hex, 16)?
    } else {
        value.parse()?
    })
}

fn parse_u8(value: &str) -> Result<u8, Box<dyn Error>> {
    Ok(if let Some(hex) = value.strip_prefix("0x") {
        u8::from_str_radix(hex, 16)?
    } else {
        value.parse()?
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 8 {
        return Err(
            "usage: iso <vid> <pid> <interface> <alt> <endpoint> <packet-count> <packet-size> <transfers>"
                .into(),
        );
    }

    let vendor_id = parse_u16(&args[0])?;
    let product_id = parse_u16(&args[1])?;
    let interface_number = parse_u8(&args[2])?;
    let alt_setting = parse_u8(&args[3])?;
    let endpoint_address = parse_u8(&args[4])?;
    let layout = IsoLayout::new(args[5].parse()?, args[6].parse()?)?;
    let transfer_count: usize = args[7].parse()?;
    if transfer_count == 0 {
        return Err("transfer count must be non-zero".into());
    }

    let device_info = nusb::list_devices()
        .wait()?
        .find(|device| device.vendor_id() == vendor_id && device.product_id() == product_id)
        .ok_or("device not found")?;
    let device = device_info.open().wait()?;
    let interface = device.detach_and_claim_interface(interface_number).wait()?;
    interface.set_alt_setting(alt_setting).wait()?;

    if endpoint_address & 0x80 != 0 {
        let endpoint = interface.endpoint::<Isochronous, In>(endpoint_address)?;
        run_in(endpoint, layout, transfer_count)
    } else {
        let endpoint = interface.endpoint::<Isochronous, Out>(endpoint_address)?;
        run_out(endpoint, layout, transfer_count)
    }
}

fn run_in(
    mut endpoint: nusb::Endpoint<Isochronous, In>,
    layout: IsoLayout,
    transfer_count: usize,
) -> Result<(), Box<dyn Error>> {
    let queue_depth = QUEUE_DEPTH.min(transfer_count);
    for _ in 0..queue_depth {
        let transfer = endpoint.allocate_iso(layout)?;
        endpoint.submit(transfer)?;
    }

    let mut total_bytes = 0usize;
    for sequence in 0..transfer_count {
        let completion = endpoint
            .wait_next_complete(Duration::from_secs(2))
            .ok_or("timed out waiting for an isochronous completion")?;
        check_completion(&completion, false)?;

        let bytes = completion.total_actual_len();
        total_bytes += bytes;
        println!("completion={sequence} bytes={bytes}");

        if sequence + queue_depth < transfer_count {
            endpoint.submit(completion.into_transfer())?;
        }
    }

    println!("received {total_bytes} bytes");
    Ok(())
}

fn run_out(
    mut endpoint: nusb::Endpoint<Isochronous, Out>,
    layout: IsoLayout,
    transfer_count: usize,
) -> Result<(), Box<dyn Error>> {
    let queue_depth = QUEUE_DEPTH.min(transfer_count);
    for sequence in 0..queue_depth {
        let mut transfer = endpoint.allocate_iso(layout)?;
        fill_out_transfer(&mut transfer, sequence as u8);
        endpoint.submit(transfer)?;
    }

    for sequence in 0..transfer_count {
        let completion = endpoint
            .wait_next_complete(Duration::from_secs(2))
            .ok_or("timed out waiting for an isochronous completion")?;
        check_completion(&completion, true)?;
        println!(
            "completion={sequence} bytes={}",
            completion.total_actual_len()
        );

        if sequence + queue_depth < transfer_count {
            let mut transfer = completion.into_transfer();
            fill_out_transfer(&mut transfer, (sequence + queue_depth) as u8);
            endpoint.submit(transfer)?;
        }
    }

    Ok(())
}

fn check_completion(
    completion: &IsoCompletion,
    allow_unreported: bool,
) -> Result<(), Box<dyn Error>> {
    completion.status()?;
    if let Some(packet) = completion.packets().find(|packet| {
        !matches!(packet.status(), IsoPacketStatus::Success)
            && !(allow_unreported && matches!(packet.status(), IsoPacketStatus::NotReported))
    }) {
        return Err(format!(
            "packet {} completed with {:?} (native status {:?})",
            packet.index(),
            packet.status(),
            packet.native_status()
        )
        .into());
    }
    Ok(())
}

fn fill_out_transfer(transfer: &mut nusb::transfer::IsoTransfer, seed: u8) {
    for index in 0..transfer.layout().packet_count() {
        transfer.packet_mut(index).unwrap().fill(seed);
    }
}
