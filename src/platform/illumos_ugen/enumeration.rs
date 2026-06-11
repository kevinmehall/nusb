use crate::descriptors::{decode_string_descriptor, ConfigurationDescriptor, DeviceDescriptor};
use crate::maybe_future::Ready;
use crate::platform::illumos_ugen::device::get_raw_string;
use crate::ErrorKind;
use crate::MaybeFuture;
use crate::{BusInfo, DeviceInfo, Error, InterfaceInfo, UsbControllerType};
use rustix::fs::{Mode, OFlags};
use std::collections::HashMap;
use std::num::NonZeroU8;

#[derive(Debug)]
// Suppress a warning about not using `Unknown`
#[allow(dead_code)]
enum PropVal {
    String(String),
    Bytes(Vec<u8>),
    Integer(i64),
    Boolean,
    Unknown(devinfo::PropType),
}

struct Hub {
    depth: u32,
    port: u8,
}

#[derive(Clone, Debug)]
pub struct DevfsPath {
    pub path: String,
    pub device_paths: HashMap<String, String>,
}

fn build_prop_tree(mut pw: devinfo::PropertyWalk) -> HashMap<String, PropVal> {
    let mut props = HashMap::new();
    while let Some(Ok(p)) = pw.next() {
        props.insert(
            p.name(),
            if let Some(val) = p.as_i64() {
                PropVal::Integer(val)
            } else if let Some(val) = p.as_bytes() {
                PropVal::Bytes(val.to_vec())
            } else if let Some(val) = p.to_str() {
                PropVal::String(val)
            } else {
                match p.value_type() {
                    devinfo::PropType::Boolean => PropVal::Boolean,
                    t => PropVal::Unknown(t),
                }
            },
        );
    }

    props
}

fn walk_buses() -> Result<impl Iterator<Item = BusInfo>, Error> {
    let mut di =
        devinfo::DevInfo::new().map_err(|_| Error::new(ErrorKind::Other, "dev info err"))?;
    let mut bus = 1;

    let mut w = di.walk_node();
    let mut buses = vec![];

    while let Some(Ok(n)) = w.next() {
        let pw = n.props();

        let props = build_prop_tree(pw);
        if let Some(PropVal::Boolean) = props.get("root-hub") {
            buses.push(BusInfo {
                driver: n.driver_name(),
                bus_id: format!("{bus:03}"),
                controller_type: n
                    .driver_name()
                    .as_ref()
                    .and_then(|p| UsbControllerType::from_str(p)),
            });
            bus += 1;
            continue;
        }
    }
    Ok(buses.into_iter())
}

fn try_get_interface_string(path: Option<&String>, index: Option<NonZeroU8>) -> Option<String> {
    let path = path?;

    let index = index?;

    let Ok(fd) = rustix::fs::open(path, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty()) else {
        return None;
    };

    let Ok(result) = get_raw_string(&fd, index.into()) else {
        return None;
    };

    decode_string_descriptor(&result).ok()
}

fn walk_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    let mut di =
        devinfo::DevInfo::new().map_err(|_| Error::new(ErrorKind::Other, "dev info err"))?;

    let mut bus = None;
    let mut hubs: Vec<Hub> = Vec::new();

    let mut w = di.walk_node();
    let mut devices = vec![];

    let links =
        devinfo::DevLinks::new(false).map_err(|_| Error::new(ErrorKind::Other, "dev links err"))?;

    while let Some(n) = w.next().transpose().unwrap() {
        let pw = n.props();
        let props = build_prop_tree(pw);
        let path = n
            .devfs_path()
            .map_err(|_| Error::new(ErrorKind::Other, "devfs path err"))?;

        if let Some(PropVal::Boolean) = props.get("root-hub") {
            bus = match bus {
                Some(bus) => Some(bus + 1),
                None => Some(0),
            };

            hubs = vec![];
            continue;
        }

        let port = match props.get("reg") {
            Some(PropVal::Integer(port)) => *port as u8,
            None => continue,
            _ => return Err(Error::new(ErrorKind::Other, "unexpected value in reg")),
        };

        let depth = n.depth();

        while hubs.last().is_some_and(|l| l.depth >= depth) {
            hubs.pop();
        }

        if n.driver_name().as_deref() == Some("hubd") {
            //
            // This is a hub.  Keep track of its assigned address.
            //
            hubs.push(Hub { depth, port });
            continue;
        }

        if let Some(PropVal::Bytes(b)) = props.get("usb-dev-descriptor") {
            let device_address = match props.get("assigned-address") {
                Some(PropVal::Integer(v)) if *v < u8::MAX.into() => *v as u8,
                _ => return Err(Error::new(ErrorKind::Other, "bad assigned-address")),
            };

            let manufacturer_string = match props.get("usb-vendor-name") {
                Some(PropVal::String(v)) => Some(v.clone()),
                None => continue,
                _ => return Err(Error::new(ErrorKind::Other, "bad usb-vendor-name")),
            };

            let product_string = match props.get("usb-product-name") {
                Some(PropVal::String(val)) => Some(val.clone()),
                None => None,
                _ => return Err(Error::new(ErrorKind::Other, "bad usb-product-name")),
            };

            let serial_number = match props.get("usb-serialno") {
                Some(PropVal::String(val)) => Some(val.clone()),
                None => None,
                _ => return Err(Error::new(ErrorKind::Other, "bad usb-serialno")),
            };

            let mut ports = hubs.iter().map(|h| h.port).collect::<Vec<_>>();
            ports.push(port);

            let Some(busnum) = bus else {
                return Err(Error::new(ErrorKind::Other, "no root port"));
            };

            if let Some(d) = DeviceDescriptor::new(b) {
                let mut paths: HashMap<String, String> = HashMap::new();
                let mut wm = n.minors();
                while let Some(m) = wm
                    .next()
                    .transpose()
                    .map_err(|_| Error::new(ErrorKind::Other, "transpose err"))?
                {
                    let minor_path = m
                        .devfs_path()
                        .map_err(|_| Error::new(ErrorKind::Other, "devfs path err"))?;

                    for link in links
                        .links_for_path(minor_path)
                        .map_err(|_| Error::new(ErrorKind::Other, "links for path err"))?
                    {
                        let lpath = link.path();

                        let file_name = match lpath.file_name() {
                            Some(file_name) => file_name.to_str().unwrap(),
                            None => return Err(Error::new(ErrorKind::Other, "bad link path")),
                        };

                        #[rustfmt::skip]
                        paths.insert(
                            file_name.to_string(),
                            lpath.to_string_lossy().into_owned()
                        );
                    }
                }

                let cntrl = paths.get("cntrl0");

                let interfaces = match props.get("usb-raw-cfg-descriptors") {
                    Some(PropVal::Bytes(cfg)) => {
                        let c = ConfigurationDescriptor::new(cfg).unwrap();

                        c.interfaces()
                            .map(|i| {
                                let alt = i.first_alt_setting();

                                //
                                // If we want to pull the interface string,
                                // we'll need to open the configuration
                                // endpoint and pull the String descriptors.
                                //
                                InterfaceInfo {
                                    interface_number: i.interface_number(),
                                    class: alt.class(),
                                    subclass: alt.subclass(),
                                    protocol: alt.protocol(),
                                    interface_string: try_get_interface_string(
                                        cntrl,
                                        alt.string_index(),
                                    ),
                                }
                            })
                            .collect::<Vec<_>>()
                    }
                    _ => return Err(Error::new(ErrorKind::Other, "bad raw config decriptors")),
                };

                devices.push(DeviceInfo {
                    path: DevfsPath {
                        path,
                        device_paths: paths,
                    },
                    bus_id: format!("{busnum:03}"),
                    busnum,
                    device_address,
                    port_chain: ports,
                    vendor_id: d.vendor_id(),
                    product_id: d.product_id(),
                    device_version: d.device_version(),
                    class: d.class(),
                    subclass: d.subclass(),
                    protocol: d.protocol(),
                    speed: None,
                    manufacturer_string,
                    product_string,
                    serial_number,
                    interfaces,
                    usb_version: d.usb_version(),
                });
            } else {
                return Err(Error::new(ErrorKind::Other, "bad device descriptor"));
            }
        }
    }

    Ok(devices.into_iter())
}

pub fn list_devices() -> impl MaybeFuture<Output = Result<impl Iterator<Item = DeviceInfo>, Error>>
{
    Ready(walk_devices())
}

pub fn list_buses() -> impl MaybeFuture<Output = Result<impl Iterator<Item = BusInfo>, Error>> {
    Ready(walk_buses())
}
