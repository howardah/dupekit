use crate::*;

pub(crate) fn time_to_millis(time: SystemTime) -> Result<i64> {
    i64::try_from(
        time.duration_since(UNIX_EPOCH)
            .map_err(|_| StorageError::InvalidTime)?
            .as_millis(),
    )
    .map_err(|_| StorageError::InvalidTime)
}
pub(crate) fn millis_to_time(value: i64) -> std::result::Result<SystemTime, rusqlite::Error> {
    u64::try_from(value)
        .map(|v| UNIX_EPOCH + std::time::Duration::from_millis(v))
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}
pub(crate) fn time_to_nanos(time: SystemTime) -> Result<i64> {
    i64::try_from(
        time.duration_since(UNIX_EPOCH)
            .map_err(|_| StorageError::InvalidTime)?
            .as_nanos(),
    )
    .map_err(|_| StorageError::InvalidTime)
}
pub(crate) fn nanos_to_time(value: i64) -> std::result::Result<SystemTime, rusqlite::Error> {
    u64::try_from(value)
        .map(|v| UNIX_EPOCH + std::time::Duration::from_nanos(v))
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}
pub(crate) fn u64_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StorageError::InvalidTime)
}
pub(crate) fn i64_to_u64(value: i64) -> std::result::Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}

#[cfg(unix)]
pub(crate) fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = vec![0];
    out.extend_from_slice(path.as_os_str().as_bytes());
    out
}
#[cfg(unix)]
pub(crate) fn decode_path(bytes: &[u8]) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    if bytes.first() != Some(&0) {
        return Err(StorageError::IncompatiblePath);
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[1..])))
}
#[cfg(windows)]
pub(crate) fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let mut out = vec![1];
    for x in path.as_os_str().encode_wide() {
        out.extend_from_slice(&x.to_le_bytes())
    }
    out
}
#[cfg(windows)]
pub(crate) fn decode_path(bytes: &[u8]) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.first() != Some(&1) || (bytes.len() - 1) % 2 != 0 {
        return Err(StorageError::IncompatiblePath);
    }
    let w = bytes[1..]
        .chunks_exact(2)
        .map(|x| u16::from_le_bytes([x[0], x[1]]))
        .collect();
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&w)))
}
