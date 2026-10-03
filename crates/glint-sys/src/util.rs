use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::PathBuf};
use windows::{Win32::System::Com::CoTaskMemFree, core::PWSTR};

pub(crate) fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub(crate) fn take_path(value: PWSTR) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let path = unsafe { std::ffi::OsString::from_wide(value.as_wide()) };
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    path.into()
}
