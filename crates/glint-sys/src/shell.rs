use anyhow::{Result, ensure};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};
use windows::{
    ApplicationModel::DataTransfer::{DataRequestedEventArgs, DataTransferManager},
    Foundation::TypedEventHandler,
    Storage::{IStorageItem, StorageFile},
    Win32::{
        Foundation::HWND,
        System::{
            Com::{CoTaskMemFree, IDataObject},
            Ole::DROPEFFECT_COPY,
        },
        UI::{
            Shell::{Common::ITEMIDLIST, *},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{HSTRING, Interface, PCWSTR, w},
};

fn execute(value: &std::ffi::OsStr) -> Result<()> {
    let value = crate::util::wide(value);
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(value.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    ensure!(
        result.0 as isize > 32,
        "ShellExecute failed with code {}",
        result.0 as isize
    );
    Ok(())
}
pub fn open_path(path: &Path) -> Result<()> {
    execute(path.as_os_str())
}
pub fn open_uri(uri: &str) -> Result<()> {
    execute(std::ffi::OsStr::new(uri))
}

struct Pidl(*mut ITEMIDLIST);
impl Pidl {
    fn from_path(path: &Path) -> Result<Self> {
        let path = crate::util::wide(std::path::absolute(path)?);
        let mut pidl = std::ptr::null_mut();
        unsafe {
            SHParseDisplayName(PCWSTR(path.as_ptr()), None, &mut pidl, 0, None)?;
        }
        Ok(Self(pidl))
    }
}
impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}

pub fn reveal_in_explorer(path: &Path) -> Result<()> {
    let pidl = Pidl::from_path(path)?;
    unsafe {
        SHOpenFolderAndSelectItems(pidl.0, None, 0)?;
    }
    Ok(())
}

/// Blocking modal OLE operation. The caller must have initialized OLE on its STA thread.
pub fn drag_files(hwnd: HWND, paths: &[PathBuf]) -> Result<()> {
    ensure!(!paths.is_empty(), "No files to drag");
    let pidls = paths
        .iter()
        .map(|path| Pidl::from_path(path))
        .collect::<Result<Vec<_>>>()?;
    let pointers: Vec<*const ITEMIDLIST> = pidls.iter().map(|pidl| pidl.0.cast_const()).collect();
    unsafe {
        let data: IDataObject = SHCreateDataObject(None, Some(&pointers), None)?;
        SHDoDragDrop(Some(hwnd), &data, None, DROPEFFECT_COPY)?;
    }
    Ok(())
}

struct ShareRegistration {
    manager: DataTransferManager,
    token: i64,
}
impl Drop for ShareRegistration {
    fn drop(&mut self) {
        let _ = self.manager.RemoveDataRequested(self.token);
    }
}
thread_local! { static SHARES: RefCell<Vec<(isize, ShareRegistration)>> = const { RefCell::new(Vec::new()) }; }

/// Opens the share sheet on the caller's WinRT STA thread. Pump messages after returning.
pub fn share_files(hwnd: HWND, paths: &[PathBuf]) -> Result<()> {
    ensure!(!paths.is_empty(), "No files to share");
    let paths = paths
        .iter()
        .map(|path| std::path::absolute(path).map(|p| HSTRING::from(p.as_os_str())))
        .collect::<std::io::Result<Vec<_>>>()?;
    let interop: IDataTransferManagerInterop =
        windows::core::factory::<DataTransferManager, IDataTransferManagerInterop>()?;
    let manager: DataTransferManager = unsafe { interop.GetForWindow(hwnd)? };
    let owner = hwnd.0 as isize;
    SHARES.with(|shares| shares.borrow_mut().retain(|(window, _)| *window != owner));
    let handler =
        TypedEventHandler::<DataTransferManager, DataRequestedEventArgs>::new(move |_, args| {
            let request = args.ok()?.Request()?;
            let deferral = request.GetDeferral()?;
            let worker_request = request.clone();
            let worker_deferral = deferral.clone();
            let paths = paths.clone();
            let worker = std::thread::Builder::new()
                .name("glint-share-files".into())
                .spawn(move || {
                    let initialized = unsafe {
                        windows::Win32::System::WinRT::RoInitialize(
                            windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
                        )
                    };
                    let populate = || -> windows::core::Result<()> {
                        initialized.clone()?;
                        let mut items = Vec::<Option<IStorageItem>>::new();
                        for path in &paths {
                            items.push(Some(
                                StorageFile::GetFileFromPathAsync(path)?.join()?.cast()?,
                            ));
                        }
                        let data = worker_request.Data()?;
                        data.Properties()?
                            .SetTitle(&HSTRING::from("Glint capture"))?;
                        let iterable: windows_collections::IIterable<IStorageItem> = items.into();
                        data.SetStorageItems(&iterable, true)
                    };
                    if let Err(error) = populate() {
                        log::error!("Share request failed: {error}");
                        let _ = worker_request.FailWithDisplayText(&HSTRING::from(
                            "Glint could not open the selected files.",
                        ));
                    }
                    let _ = worker_deferral.Complete();
                    drop(worker_request);
                    drop(worker_deferral);
                    if initialized.is_ok() {
                        unsafe {
                            windows::Win32::System::WinRT::RoUninitialize();
                        }
                    }
                });
            if let Err(error) = worker {
                log::error!("Could not start share worker: {error}");
                let _ = request.FailWithDisplayText(&HSTRING::from(
                    "Glint could not start the share request.",
                ));
                deferral.Complete()?;
            }
            Ok(())
        });
    let token = manager.DataRequested(&handler)?;
    SHARES.with(|shares| {
        shares
            .borrow_mut()
            .push((owner, ShareRegistration { manager, token }))
    });
    if let Err(error) = unsafe { interop.ShowShareUIForWindow(hwnd) } {
        SHARES.with(|shares| shares.borrow_mut().retain(|(window, _)| *window != owner));
        return Err(error.into());
    }
    Ok(())
}

/// Release a window's share registration when the owning window is destroyed.
pub fn release_share_window(hwnd: HWND) {
    SHARES.with(|shares| {
        shares
            .borrow_mut()
            .retain(|(window, _)| *window != hwnd.0 as isize)
    });
}
