use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY};

/// Best effort: a refused priority change just leaves the thread at its current priority.
pub fn set_current_priority(priority: THREAD_PRIORITY) {
    // SAFETY: GetCurrentThread returns a pseudo handle that is always valid for the calling thread.
    if let Err(error) = unsafe { SetThreadPriority(GetCurrentThread(), priority) } {
        log::debug!("SetThreadPriority({}): {error}", priority.0);
    }
}
