use glint_core::ImageFormat;
use std::path::{Path, PathBuf};
use windows::{
    Win32::UI::Shell::{
        FDE_OVERWRITE_RESPONSE, FDE_SHAREVIOLATION_RESPONSE, FDEOR_DEFAULT, FDESVR_DEFAULT,
        IFileDialog, IFileDialogEvents, IFileDialogEvents_Impl,
    },
    core::{Ref, implement},
};
use windows::{
    Win32::{
        Foundation::HWND,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::Shell::{
            Common::COMDLG_FILTERSPEC, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST,
            FOS_PICKFOLDERS, FOS_STRICTFILETYPES, FileOpenDialog, FileSaveDialog, IFileOpenDialog,
            IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
        },
    },
    core::{PCWSTR, w},
};

#[implement(IFileDialogEvents)]
struct ImageDialogEvents;
impl IFileDialogEvents_Impl for ImageDialogEvents_Impl {
    fn OnFileOk(&self, _: Ref<IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnFolderChanging(
        &self,
        _: Ref<IFileDialog>,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnFolderChange(&self, _: Ref<IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnSelectionChange(&self, _: Ref<IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnShareViolation(
        &self,
        _: Ref<IFileDialog>,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<FDE_SHAREVIOLATION_RESPONSE> {
        Ok(FDESVR_DEFAULT)
    }
    fn OnTypeChange(&self, dialog: Ref<IFileDialog>) -> windows::core::Result<()> {
        if let Some(dialog) = dialog.as_ref() {
            unsafe {
                let extension = if dialog.GetFileTypeIndex()? == 2 {
                    "jpg"
                } else {
                    "png"
                };
                let current = dialog.GetFileName()?;
                let mut path = crate::util::take_path(current);
                path.set_extension(extension);
                let name = crate::util::wide(path);
                let extension = crate::util::wide(extension);
                dialog.SetDefaultExtension(PCWSTR(extension.as_ptr()))?;
                dialog.SetFileName(PCWSTR(name.as_ptr()))?;
            }
        }
        Ok(())
    }
    fn OnOverwrite(
        &self,
        _: Ref<IFileDialog>,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<FDE_OVERWRITE_RESPONSE> {
        Ok(FDEOR_DEFAULT)
    }
}

fn shell_item(path: &Path) -> windows::core::Result<IShellItem> {
    let path = crate::util::wide(path);
    unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None) }
}

/// The caller must have initialized a COM STA apartment.
pub fn save_image_dialog(
    owner: HWND,
    default_dir: &Path,
    default_name: &str,
    format: ImageFormat,
) -> Option<(PathBuf, ImageFormat)> {
    let show = || -> windows::core::Result<(PathBuf, ImageFormat)> {
        unsafe {
            let dialog: IFileSaveDialog =
                CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
            dialog.SetOptions(
                dialog.GetOptions()?
                    | FOS_FORCEFILESYSTEM
                    | FOS_PATHMUSTEXIST
                    | FOS_OVERWRITEPROMPT
                    | FOS_STRICTFILETYPES,
            )?;
            dialog.SetFileTypes(&[
                COMDLG_FILTERSPEC {
                    pszName: w!("PNG image"),
                    pszSpec: w!("*.png"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("JPEG image"),
                    pszSpec: w!("*.jpg"),
                },
            ])?;
            dialog.SetFileTypeIndex(if format == ImageFormat::Png { 1 } else { 2 })?;
            dialog.SetDefaultExtension(if format == ImageFormat::Png {
                w!("png")
            } else {
                w!("jpg")
            })?;
            let default_stem = Path::new(default_name)
                .file_stem()
                .unwrap_or_else(|| std::ffi::OsStr::new(default_name));
            let name = crate::util::wide(default_stem);
            dialog.SetFileName(PCWSTR(name.as_ptr()))?;
            if let Ok(folder) = shell_item(default_dir) {
                dialog.SetDefaultFolder(&folder)?;
            }
            let events: IFileDialogEvents = ImageDialogEvents.into();
            let cookie = dialog.Advise(&events)?;
            let shown = dialog.Show(Some(owner));
            let _ = dialog.Unadvise(cookie);
            shown?;
            let selected = if dialog.GetFileTypeIndex()? == 2 {
                ImageFormat::Jpeg
            } else {
                ImageFormat::Png
            };
            let mut path =
                crate::util::take_path(dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?);
            path.set_extension(if selected == ImageFormat::Png {
                "png"
            } else {
                "jpg"
            });
            Ok((path, selected))
        }
    };
    show().ok()
}

/// The caller must have initialized a COM STA apartment.
pub fn pick_folder(owner: HWND, initial: &Path) -> Option<PathBuf> {
    let show = || -> windows::core::Result<PathBuf> {
        unsafe {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            dialog.SetOptions(
                dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
            )?;
            if let Ok(folder) = shell_item(initial) {
                dialog.SetFolder(&folder)?;
            }
            dialog.Show(Some(owner))?;
            Ok(crate::util::take_path(
                dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?,
            ))
        }
    };
    show().ok()
}
