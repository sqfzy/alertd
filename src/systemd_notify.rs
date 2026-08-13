#[cfg(target_os = "linux")]
pub fn send(message: &str) -> std::io::Result<()> {
    use std::{env, mem, os::unix::ffi::OsStrExt};

    let Some(socket) = env::var_os("NOTIFY_SOCKET") else {
        return Ok(());
    };
    let bytes = socket.as_bytes();
    if bytes.is_empty() {
        return Ok(());
    }
    let descriptor =
        unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut address: libc::sockaddr_un = unsafe { mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let offset = mem::offset_of!(libc::sockaddr_un, sun_path);
    let path_bytes = if bytes[0] == b'@' { &bytes[1..] } else { bytes };
    if path_bytes.len() + 1 > address.sun_path.len() {
        unsafe { libc::close(descriptor) };
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NOTIFY_SOCKET path is too long",
        ));
    }
    let abstract_socket = bytes[0] == b'@';
    let start = usize::from(abstract_socket);
    for (index, byte) in path_bytes.iter().enumerate() {
        address.sun_path[start + index] = *byte as libc::c_char;
    }
    let length = offset + path_bytes.len() + 1;
    let sent = unsafe {
        libc::sendto(
            descriptor,
            message.as_ptr().cast(),
            message.len(),
            libc::MSG_NOSIGNAL,
            (&raw const address).cast(),
            length as libc::socklen_t,
        )
    };
    let result = if sent < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    };
    unsafe { libc::close(descriptor) };
    result
}

#[cfg(not(target_os = "linux"))]
pub fn send(_message: &str) -> std::io::Result<()> {
    Ok(())
}
