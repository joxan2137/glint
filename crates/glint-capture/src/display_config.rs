use std::mem::size_of;

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
    QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID};

use crate::wide;

const DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(14);
const SDR_REFERENCE_WHITE_NITS: f32 = 80.0;
const QUERY_ATTEMPTS: usize = 3;

/// `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2` (Windows 11 24H2); not yet in the `windows` bindings.
#[repr(C)]
struct AdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    flags: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    Sdr,
    WideColor,
    Hdr,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdvancedColor {
    pub supported: bool,
    pub active: bool,
    pub hdr_supported: bool,
    pub hdr_user_enabled: bool,
    pub wide_color_supported: bool,
    pub wide_color_user_enabled: bool,
    pub limited_by_policy: bool,
    pub bits_per_channel: u32,
    pub active_mode: Option<ColorMode>,
}

impl AdvancedColor {
    fn from_info_2(info: &AdvancedColorInfo2) -> Self {
        let bit = |index: u32| info.flags & (1 << index) != 0;
        Self {
            supported: bit(0),
            active: bit(1),
            limited_by_policy: bit(3),
            hdr_supported: bit(4),
            hdr_user_enabled: bit(5),
            wide_color_supported: bit(6),
            wide_color_user_enabled: bit(7),
            bits_per_channel: info.bits_per_color_channel,
            active_mode: match info.active_color_mode {
                0 => Some(ColorMode::Sdr),
                1 => Some(ColorMode::WideColor),
                2 => Some(ColorMode::Hdr),
                _ => None,
            },
        }
    }

    fn from_info_1(info: &DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO) -> Self {
        // SAFETY: both union members are plain u32 views of the same flag word.
        let flags = unsafe { info.Anonymous.value };
        Self {
            supported: flags & 1 != 0,
            active: flags & 2 != 0,
            limited_by_policy: flags & 8 != 0,
            bits_per_channel: info.bitsPerColorChannel,
            ..Self::default()
        }
    }
}

/// What the display configuration knows about one active monitor, keyed by its GDI source name.
#[derive(Clone, Debug)]
pub struct DisplayTarget {
    pub gdi_device_name: String,
    pub friendly_name: Option<String>,
    pub sdr_white_nits: Option<f32>,
    pub advanced_color: Option<AdvancedColor>,
}

pub fn active_targets() -> Vec<DisplayTarget> {
    let Some(paths) = active_paths() else {
        log::warn!("QueryDisplayConfig failed; monitor names and SDR white levels unavailable");
        return Vec::new();
    };
    paths.iter().filter_map(describe_path).collect()
}

fn active_paths() -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
    for _ in 0..QUERY_ATTEMPTS {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        // SAFETY: both counts are valid out pointers.
        let sizes = unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) };
        if sizes != ERROR_SUCCESS {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        // SAFETY: the buffers hold exactly the element counts passed in.
        let result = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if result == ERROR_SUCCESS {
            paths.truncate(path_count as usize);
            return Some(paths);
        }
        if result != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
    }
    None
}

fn describe_path(path: &DISPLAYCONFIG_PATH_INFO) -> Option<DisplayTarget> {
    let source_id = (path.sourceInfo.adapterId, path.sourceInfo.id);
    let target_id = (path.targetInfo.adapterId, path.targetInfo.id);

    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, source_id),
        ..Default::default()
    };
    if !request(&mut source) {
        return None;
    }

    let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, target_id),
        ..Default::default()
    };
    let friendly_name = request(&mut name)
        .then(|| wide::to_string(&name.monitorFriendlyDeviceName))
        .filter(|name| !name.is_empty());

    let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
        header: header::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, target_id),
        ..Default::default()
    };
    let sdr_white_nits = request(&mut white)
        .then(|| white.SDRWhiteLevel as f32 / 1000.0 * SDR_REFERENCE_WHITE_NITS)
        .filter(|nits| *nits > 0.0);

    Some(DisplayTarget {
        gdi_device_name: wide::to_string(&source.viewGdiDeviceName),
        friendly_name,
        sdr_white_nits,
        advanced_color: advanced_color(target_id),
    })
}

fn advanced_color(target_id: (LUID, u32)) -> Option<AdvancedColor> {
    let mut info_2 = AdvancedColorInfo2 {
        header: header::<AdvancedColorInfo2>(DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2, target_id),
        flags: 0,
        color_encoding: 0,
        bits_per_color_channel: 0,
        active_color_mode: 0,
    };
    if request(&mut info_2) {
        return Some(AdvancedColor::from_info_2(&info_2));
    }
    let mut info_1 = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: header::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            target_id,
        ),
        ..Default::default()
    };
    request(&mut info_1).then(|| AdvancedColor::from_info_1(&info_1))
}

fn header<T>(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, (adapter_id, id): (LUID, u32)) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size_of::<T>() as u32, adapterId: adapter_id, id }
}

/// `packet` must be a `#[repr(C)]` struct that starts with a filled-in `DISPLAYCONFIG_DEVICE_INFO_HEADER`.
fn request<T>(packet: &mut T) -> bool {
    // SAFETY: every caller passes a repr(C) packet whose first field is the header and whose header.size is size_of::<T>().
    unsafe { DisplayConfigGetDeviceInfo((packet as *mut T).cast()) == ERROR_SUCCESS.0 as i32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_with(flags: u32, mode: i32) -> AdvancedColorInfo2 {
        AdvancedColorInfo2 {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER::default(),
            flags,
            color_encoding: 0,
            bits_per_color_channel: 10,
            active_color_mode: mode,
        }
    }

    #[test]
    fn decodes_hdr_active_flags() {
        let color = AdvancedColor::from_info_2(&info_with(0b0011_0011, 2));
        assert!(color.supported && color.active && color.hdr_supported && color.hdr_user_enabled);
        assert!(!color.wide_color_supported && !color.limited_by_policy);
        assert_eq!(color.active_mode, Some(ColorMode::Hdr));
        assert_eq!(color.bits_per_channel, 10);
    }

    #[test]
    fn unknown_mode_is_none() {
        assert_eq!(AdvancedColor::from_info_2(&info_with(0, 9)).active_mode, None);
    }
}
