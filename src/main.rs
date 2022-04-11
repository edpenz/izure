use std::collections::HashMap;
use std::env;
use std::io::{self, Read, Write};
use std::time::Duration;
use std::net::{ToSocketAddrs, TcpStream};
use std::os::unix::io::AsRawFd;

use libc::{self};

/// Sends a chunk of data from one FD to another
fn drain<A: AsRawFd, B: AsRawFd>(from: & A, to: & B) -> io::Result<usize> {
    let from_fd = from.as_raw_fd();
    let to_fd = to.as_raw_fd();
    let null_offset = std::ptr::null_mut::<libc::loff_t>();

    let result = unsafe {
        libc::splice(
            from_fd, null_offset,
            to_fd, null_offset,
            libc::PIPE_BUF,
            0)
    };
    if result >= 0 {
        return Ok(result as usize);
    }

    let errno = unsafe { *libc::__errno_location() };
    return Err(io::Error::from_raw_os_error(errno));
}

fn _drain<A: Read, B: Write>(from: &mut A, to: &mut B) -> io::Result<usize> {
    let mut buffer = [0 as u8; libc::PIPE_BUF];
    match from.read(&mut buffer) {
        Ok(len) => to.write(&buffer[0..len]),
        err => err,
    }
}

fn write_line(tty: &mut std::fs::File, line: &[u8]) -> io::Result<usize> {
    tty.write(b"\x1b[2K\r").and_then(|_| tty.write(line))
}

fn main() {
    let mut args = env::args();

    // Discard the program name argument
    args.next();

    // Parse args
    let host = args.next().expect("Mising host argument");
    let port = args.next().map_or(22, |s| s.parse::<u16>().expect("Given port is not valid"));

    // Get access to terminal, bypassing redirected stdout
    let mut tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").expect("Could not open TTY");

    // Resolve host name to IP
    write_line(&mut tty, b"DNS, ...").unwrap();
    let host_port = (host, port);
    let addr = loop {
        match host_port.to_socket_addrs() {
            Ok(mut addrs) => break addrs.next().unwrap(),
            Err(err) => {
                write_line(&mut tty, format!("DNS, {}", err).as_bytes()).unwrap();
                std::thread::sleep(Duration::from_millis(1000))
            },
        };
    };
    
    // Open TCP connection
    write_line(&mut tty, b"TCP, ...").unwrap();
    let connection = loop {
        match TcpStream::connect_timeout(&addr, Duration::from_millis(1000)) {
            Ok(connection) => break connection,
            Err(err) => {
                write_line(&mut tty, format!("TCP, {}", err).as_bytes()).unwrap();
                std::thread::sleep(Duration::from_millis(1000))
            },
        };
    };
    let _ = connection.set_nodelay(true);
    
    // Clear TTY output now that connection is up.
    write_line(&mut tty, b"").unwrap();
    
    // Get handles for stdin/stdout pipes
    // TODO: Make sure the way we do this avoids rust re-locking them on every access
    // TODO: Fallback to alternative drain function if they are not pipes?
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    // Setup for FD polling
    let mut poll_fds = [
        libc::pollfd { fd: stdin.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: connection.as_raw_fd(), events: libc::POLLIN, revents: 0 },
    ];

    let fd_mapping = HashMap::from([
        (stdin.as_raw_fd(), connection.as_raw_fd() ),
        (connection.as_raw_fd(), stdout.as_raw_fd() ),
    ]);
    
    // Stream data
    while (poll_fds[0].events != 0) && (poll_fds[1].events != 0) {
        unsafe { libc::poll(&mut poll_fds[0], poll_fds.len()as libc::nfds_t, -1); }
        
        for poll_fd in &mut poll_fds {
            let src_fd = poll_fd.fd;
            let dst_fd = fd_mapping[&src_fd];

            // TODO: Properly handle simultaneous events (other than POLLHUP and POLLIN).
            // Filter down to a single event (preferring read/write over errors)
            let event_bit = poll_fd.revents & !(poll_fd.revents - 1);

            // TODO: Also half-close TCP stream on errors?
            match event_bit {
                0 => {},
                libc::POLLIN => {
                    match drain(&src_fd, &dst_fd) {
                        Ok(0) => {
                            // FIXME: Need to actually remove from poll array (due to continued POLLHUP events)
                            poll_fd.events = 0;
                            // TODO: Print error (in debug mode only?)
                            // write!(&mut tty, "{} EOF\r\n", src_fd);
                        },
                        Ok(_n) => {
                            // TODO: Print event in debug mode only
                            // write!(&mut tty, "{} -> {}: {}b\r\n", src_fd, dst_fd, _n);
                        },
                        Err(_err) => {
                            poll_fd.events = 0;
                            // TODO: Print error (in debug mode only?)
                            // write!(&mut tty, "drain(...) failed: {}\r\n", _err);
                        }
                    }
                },
                _n => {
                    // TODO: Print error (in debug mode only?)
                    // write!(&mut tty, "poll(...) returned {}\r\n", _n);
                    return;
                },
            }
        }
    }
}
