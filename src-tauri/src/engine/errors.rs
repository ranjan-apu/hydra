//! Human error strings.
//!
//! The engine must never surface raw OS text to the user ("io error: No space
//! left on device (os error 28)"), so every I/O failure that can reach a state
//! event passes through here first. The output strings are lowercase and are
//! shown verbatim by the UI (CONTRACT "Error strings").

use std::io::ErrorKind;

/// POSIX errno values we care about. They are identical on macOS and Linux.
const ENOENT: i32 = 2;
const EACCES: i32 = 13;
const ENOSPC: i32 = 28;
const EROFS: i32 = 30;

/// True when the failure means "the disk (not the network) is the problem".
pub fn is_disk_full(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(ENOSPC) | Some(EROFS)) || err.kind() == ErrorKind::StorageFull
}

/// True when the failure is a permission problem on the destination.
pub fn is_permission_denied(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(EACCES)) || err.kind() == ErrorKind::PermissionDenied
}

/// Map a local disk failure onto a CONTRACT error string.
///
/// `dest` is the destination directory, used only for the "not writable"
/// wording (the one place a path is allowed to reach the user).
pub fn write_error(dest: Option<&std::path::Path>, err: &std::io::Error) -> String {
    if is_disk_full(err) {
        return "disk full".to_string();
    }
    if is_permission_denied(err) {
        return match dest {
            Some(d) => format!("destination is not writable: {}", d.display()),
            None => "destination is not writable".to_string(),
        };
    }
    if matches!(err.raw_os_error(), Some(ENOENT)) || err.kind() == ErrorKind::NotFound {
        return "the destination folder disappeared".to_string();
    }
    "could not write to disk".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Error;

    fn os_err(code: i32) -> Error {
        Error::from_raw_os_error(code)
    }

    #[test]
    fn enospc_is_disk_full() {
        assert_eq!(write_error(None, &os_err(ENOSPC)), "disk full");
    }

    #[test]
    fn read_only_filesystem_is_disk_full() {
        assert_eq!(write_error(None, &os_err(EROFS)), "disk full");
    }

    #[test]
    fn eacces_names_the_destination() {
        let dest = std::path::Path::new("/Users/someone/Downloads");
        assert_eq!(
            write_error(Some(dest), &os_err(EACCES)),
            "destination is not writable: /Users/someone/Downloads"
        );
        assert_eq!(
            write_error(None, &os_err(EACCES)),
            "destination is not writable"
        );
    }

    #[test]
    fn enoent_is_reported_without_a_path() {
        assert_eq!(
            write_error(None, &os_err(ENOENT)),
            "the destination folder disappeared"
        );
    }

    #[test]
    fn unknown_io_is_still_human() {
        let e = Error::other("some internal detail");
        let msg = write_error(None, &e);
        assert_eq!(msg, "could not write to disk");
        assert!(!msg.contains("os error"));
        assert!(!msg.contains("some internal detail"));
    }

    #[test]
    fn classification_helpers_agree() {
        assert!(is_disk_full(&os_err(ENOSPC)));
        assert!(is_permission_denied(&os_err(EACCES)));
        assert!(!is_disk_full(&os_err(EACCES)));
        assert!(!is_permission_denied(&os_err(ENOSPC)));
    }
}
