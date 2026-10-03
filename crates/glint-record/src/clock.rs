use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer, TIMER_ALL_ACCESS,
    WaitForSingleObject,
};
use windows::core::PCWSTR;

use crate::audio::SAMPLE_RATE;

/// Media Foundation and WASAPI both count time in 100 ns units ("hns").
pub(crate) const HNS_PER_SECOND: i64 = 10_000_000;

/// QPC time in 100 ns units, the scale WASAPI reports packet QPC positions in.
pub(crate) fn now_hns() -> i64 {
    static FREQUENCY: OnceLock<i64> = OnceLock::new();
    let frequency = *FREQUENCY.get_or_init(|| {
        let mut frequency = 0;
        let _ = unsafe { QueryPerformanceFrequency(&mut frequency) };
        frequency.max(1)
    });
    let mut counter = 0;
    let _ = unsafe { QueryPerformanceCounter(&mut counter) };
    (counter as i128 * HNS_PER_SECOND as i128 / frequency as i128) as i64
}

/// Sub-millisecond sleeps via a high-resolution waitable timer; falls back to `thread::sleep`.
pub(crate) struct PreciseSleeper(Option<HANDLE>);

impl PreciseSleeper {
    pub fn new() -> Self {
        let timer = unsafe {
            CreateWaitableTimerExW(None, PCWSTR::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)
        };
        Self(timer.ok())
    }

    pub fn sleep_hns(&self, duration: i64) {
        let duration = duration.max(0);
        if let Some(timer) = self.0 {
            let relative_due_time = -duration;
            if unsafe { SetWaitableTimer(timer, &relative_due_time, 0, None, None, false) }.is_ok() {
                unsafe { WaitForSingleObject(timer, INFINITE) };
                return;
            }
        }
        std::thread::sleep(Duration::from_nanos(duration as u64 * 100));
    }
}

impl Drop for PreciseSleeper {
    fn drop(&mut self) {
        if let Some(timer) = self.0.take() {
            let _ = unsafe { CloseHandle(timer) };
        }
    }
}

/// Maps wall-clock QPC time onto the recording's media time, with paused spans removed.
#[derive(Debug, Default)]
pub(crate) struct Timeline {
    start: Option<i64>,
    paused_since: Option<i64>,
    pauses: Vec<(i64, i64)>,
    stopped_at: Option<i64>,
}

impl Timeline {
    pub fn start(&mut self, now: i64) {
        self.start.get_or_insert(now);
    }

    pub fn is_started(&self) -> bool {
        self.start.is_some()
    }

    pub fn pause(&mut self, now: i64) {
        if self.paused_since.is_none() && self.stopped_at.is_none() {
            self.paused_since = Some(now);
        }
    }

    pub fn resume(&mut self, now: i64) {
        if let Some(since) = self.paused_since.take() {
            self.pauses.push((since, now.max(since)));
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused_since.is_some()
    }

    pub fn stop(&mut self, now: i64) {
        self.stopped_at.get_or_insert(now);
    }

    /// Media time reached at wall time `now`; 0 before the start, frozen while paused and after stop.
    pub fn media_time(&self, now: i64) -> i64 {
        let Some(start) = self.start else {
            return 0;
        };
        let end = self.stopped_at.map_or(now, |stop| now.min(stop)).max(start);
        let paused: i64 = self.pause_spans().map(|(from, to)| (to.min(end) - from.max(start)).max(0)).sum();
        end - start - paused
    }

    /// Media time of something captured at wall time `captured_at`, or None when that moment is not part of
    /// the recording (before the start, while paused, after stop).
    pub fn media_time_of_capture(&self, captured_at: i64) -> Option<i64> {
        let start = self.start?;
        let outside = captured_at < start
            || self.stopped_at.is_some_and(|stop| captured_at > stop)
            || self.pause_spans().any(|(from, to)| captured_at >= from && captured_at < to);
        (!outside).then(|| self.media_time(captured_at))
    }

    fn pause_spans(&self) -> impl Iterator<Item = (i64, i64)> + '_ {
        self.pauses.iter().copied().chain(self.paused_since.map(|since| (since, i64::MAX)))
    }
}

pub(crate) fn hns_to_duration(hns: i64) -> Duration {
    Duration::from_nanos(hns.max(0) as u64 * 100)
}

/// Presentation time of video frame `index` at a constant frame rate.
pub(crate) fn frame_time(index: u64, fps: u32) -> i64 {
    (index as i128 * HNS_PER_SECOND as i128 / fps as i128) as i64
}

/// Number of frames whose presentation time (`frame_time`) is at or before `media_time`.
pub(crate) fn frames_due(media_time: i64, fps: u32) -> u64 {
    if media_time < 0 {
        return 0;
    }
    (((media_time as i128 + 1) * fps as i128 - 1) / HNS_PER_SECOND as i128) as u64 + 1
}

/// Index of the 48 kHz audio frame playing at `media_time`.
pub(crate) fn audio_frame_at(media_time: i64) -> u64 {
    (media_time.max(0) as i128 * SAMPLE_RATE as i128 / HNS_PER_SECOND as i128) as u64
}

pub(crate) fn audio_frame_time(index: u64) -> i64 {
    (index as i128 * HNS_PER_SECOND as i128 / SAMPLE_RATE as i128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = HNS_PER_SECOND;

    #[test]
    fn media_time_is_zero_before_start() {
        let timeline = Timeline::default();
        assert_eq!(timeline.media_time(5 * S), 0);
        assert_eq!(timeline.media_time_of_capture(5 * S), None);
    }

    #[test]
    fn pauses_are_removed_from_media_time() {
        let mut timeline = Timeline::default();
        timeline.start(10 * S);
        timeline.pause(11 * S);
        assert!(timeline.is_paused());
        assert_eq!(timeline.media_time(12 * S), S);
        timeline.resume(13 * S);
        assert_eq!(timeline.media_time(13 * S), S);
        assert_eq!(timeline.media_time(14 * S), 2 * S);
        timeline.pause(15 * S);
        timeline.resume(16 * S);
        assert_eq!(timeline.media_time(17 * S), 4 * S);
    }

    #[test]
    fn pause_before_start_only_counts_after_start() {
        let mut timeline = Timeline::default();
        timeline.pause(S);
        timeline.start(2 * S);
        timeline.resume(3 * S);
        assert_eq!(timeline.media_time(4 * S), S);
    }

    #[test]
    fn stop_freezes_media_time() {
        let mut timeline = Timeline::default();
        timeline.start(0);
        timeline.stop(3 * S);
        assert_eq!(timeline.media_time(10 * S), 3 * S);
        timeline.pause(4 * S);
        assert!(!timeline.is_paused());
    }

    #[test]
    fn captures_inside_pauses_are_dropped() {
        let mut timeline = Timeline::default();
        timeline.start(0);
        timeline.pause(S);
        timeline.resume(2 * S);
        assert_eq!(timeline.media_time_of_capture(S / 2), Some(S / 2));
        assert_eq!(timeline.media_time_of_capture(S + S / 2), None);
        assert_eq!(timeline.media_time_of_capture(2 * S + S / 2), Some(S + S / 2));
        assert_eq!(timeline.media_time_of_capture(-1), None);
        timeline.stop(3 * S);
        assert_eq!(timeline.media_time_of_capture(4 * S), None);
    }

    #[test]
    fn frame_schedule_is_constant_rate() {
        assert_eq!(frame_time(0, 60), 0);
        assert_eq!(frame_time(60, 60), S);
        assert_eq!(frame_time(1, 60), 166_666);
        assert_eq!(frames_due(0, 30), 1);
        assert_eq!(frames_due(frame_time(1, 30) - 1, 30), 1);
        assert_eq!(frames_due(frame_time(1, 30), 30), 2);
        assert_eq!(frames_due(3 * S, 30), 91);
        assert_eq!(frames_due(-1, 30), 0);
        for index in 0..200 {
            assert_eq!(frames_due(frame_time(index, 60), 60), index + 1);
            assert_eq!(frames_due(frame_time(index + 1, 60) - 1, 60), index + 1);
        }
    }

    #[test]
    fn audio_frames_track_media_time() {
        assert_eq!(audio_frame_at(S), 48_000);
        assert_eq!(audio_frame_at(-S), 0);
        assert_eq!(audio_frame_time(48_000), S);
        assert_eq!(audio_frame_at(audio_frame_time(12_345)), 12_345);
    }

    #[test]
    fn precise_sleeper_sleeps_about_as_long_as_asked() {
        let sleeper = PreciseSleeper::new();
        let before = now_hns();
        sleeper.sleep_hns(20_000);
        let slept = now_hns() - before;
        assert!((20_000..200_000).contains(&slept), "{slept}");
    }

    #[test]
    fn qpc_clock_advances() {
        let a = now_hns();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = now_hns();
        assert!(b - a >= 40_000, "{a} -> {b}");
    }
}
