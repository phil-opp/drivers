//#![deny(warnings)]

extern crate syscall;
extern crate byteorder;

use std::{env, usize};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use syscall::{EVENT_READ, PHYSMAP_NO_CACHE, PHYSMAP_WRITE, Event, Packet, SchemeBlockMut};

use scheme::DiskScheme;

pub mod ahci;
pub mod scheme;

fn main() {
    let mut args = env::args().skip(1);

    let mut name = args.next().expect("ahcid: no name provided");
    name.push_str("_ahci");

    let bar_str = args.next().expect("ahcid: no address provided");
    let bar = usize::from_str_radix(&bar_str, 16).expect("ahcid: failed to parse address");

    let irq_str = args.next().expect("ahcid: no irq provided");
    let irq = irq_str.parse::<u8>().expect("ahcid: failed to parse irq");

    print!("{}", format!(" + AHCI {} on: {:X} IRQ: {}\n", name, bar, irq));

    // Daemonize
    if unsafe { syscall::clone(0).unwrap() } == 0 {
        let address = unsafe { syscall::physmap(bar, 4096, PHYSMAP_WRITE | PHYSMAP_NO_CACHE).expect("ahcid: failed to map address") };
        {
            let scheme_name = format!("disk/{}", name);
            let socket_fd = syscall::open(
                &format!(":{}", scheme_name),
                syscall::O_RDWR | syscall::O_CREAT | syscall::O_NONBLOCK
            ).expect("ahcid: failed to create disk scheme");
            let mut socket = unsafe { File::from_raw_fd(socket_fd) };

            let irq_fd = syscall::open(
                &format!("irq:{}", irq),
                syscall::O_RDWR | syscall::O_NONBLOCK
            ).expect("ahcid: failed to open irq file");
            let mut irq_file = unsafe { File::from_raw_fd(irq_fd) };

            let mut event_file = File::open("event:").expect("ahcid: failed to open event file");

            syscall::setrens(0, 0).expect("ahcid: failed to enter null namespace");

            event_file.write(&Event {
                id: socket_fd,
                flags: EVENT_READ,
                data: 0
            }).expect("ahcid: failed to event disk scheme");

            event_file.write(&Event {
                id: irq_fd,
                flags: EVENT_READ,
                data: 0
            }).expect("ahcid: failed to event irq scheme");

            let (hba_mem, disks) = ahci::disks(address, &name);
            let mut scheme = DiskScheme::new(scheme_name, hba_mem, disks);

            let mut todo = Vec::new();
            loop {
                let mut event = Event::default();
                if event_file.read(&mut event).expect("ahcid: failed to read event file") == 0 {
                    break;
                }
                if event.id == socket_fd {
                    loop {
                        let mut packet = Packet::default();
                        if socket.read(&mut packet).expect("ahcid: failed to read disk scheme") == 0 {
                            break;
                        }

                        if let Some(a) = scheme.handle(&packet) {
                            packet.a = a;
                            socket.write(&mut packet).expect("ahcid: failed to write disk scheme");
                        } else {
                            todo.push(packet);
                        }
                    }
                } else if event.id == irq_fd {
                    let mut irq = [0; 8];
                    if irq_file.read(&mut irq).expect("ahcid: failed to read irq file") >= irq.len() {
                        if scheme.irq() {
                            irq_file.write(&irq).expect("ahcid: failed to write irq file");

                            // Handle todos in order to finish previous packets if possible
                            let mut i = 0;
                            while i < todo.len() {
                                if let Some(a) = scheme.handle(&todo[i]) {
                                    let mut packet = todo.remove(i);
                                    packet.a = a;
                                    socket.write(&mut packet).expect("ahcid: failed to write disk scheme");
                                } else {
                                    i += 1;
                                }
                            }
                        }
                    }
                } else {
                    println!("Unknown event {}", event.id);
                }

                // Handle todos to start new packets if possible
                let mut i = 0;
                while i < todo.len() {
                    if let Some(a) = scheme.handle(&todo[i]) {
                        let mut packet = todo.remove(i);
                        packet.a = a;
                        socket.write(&mut packet).expect("ahcid: failed to write disk scheme");
                    } else {
                        i += 1;
                    }
                }
            }
        }
        unsafe { let _ = syscall::physunmap(address); }
    }
}
