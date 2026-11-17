//! Paths shared by the daemon and the client.

/// Pure resolution from the relevant environment values.
pub fn socket_path_from(logid_socket: Option<&str>, runtime_dir: Option<&str>, uid: u32) -> String {
    if let Some(p) = logid_socket.filter(|p| !p.is_empty()) {
        return p.to_string();
    }
    match runtime_dir.filter(|d| !d.is_empty()) {
        Some(dir) => format!("{}/logid.sock", dir.trim_end_matches('/')),
        None => format!("/tmp/logid-{uid}.sock"),
    }
}

pub fn default_socket_path() -> String {
    // SAFETY: getuid never fails and has no preconditions.
    let uid = unsafe { libc::getuid() };
    socket_path_from(
        std::env::var("LOGID_SOCKET").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        uid,
    )
}

#[cfg(test)]
mod tests {
    use super::socket_path_from;

    #[test]
    fn precedence() {
        assert_eq!(
            socket_path_from(Some("/a.sock"), Some("/run/user/1"), 1),
            "/a.sock"
        );
        assert_eq!(
            socket_path_from(None, Some("/run/user/1"), 1),
            "/run/user/1/logid.sock"
        );
        assert_eq!(
            socket_path_from(Some(""), None, 1000),
            "/tmp/logid-1000.sock"
        );
    }
}
