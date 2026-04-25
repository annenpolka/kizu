use std::path::PathBuf;

/// Convert raw filesystem bytes coming out of git into a `PathBuf`.
/// On Unix this preserves non-UTF8 filenames byte-for-byte via
/// [`std::os::unix::ffi::OsStrExt`]. On other platforms we fall back
/// to a lossy UTF-8 decode, which covers every filename people actually
/// ship but can corrupt genuinely invalid byte sequences on Windows.
#[cfg(unix)]
pub(super) fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
pub(super) fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}
