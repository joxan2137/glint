use std::collections::VecDeque;

/// Loopback packets carry the QPC time they will be heard, often well in the future. Whether that moment is
/// inside the recording (started, not paused) is only known once it has passed, so packets wait here until then.
const MAX_LEAD_HNS: i64 = 10_000_000;

pub(crate) struct TimedSamples {
    /// QPC time (100 ns units) of the first frame.
    pub timestamp: i64,
    /// Interleaved 48 kHz stereo.
    pub samples: Vec<f32>,
}

#[derive(Default)]
pub(crate) struct PlayoutQueue {
    waiting: VecDeque<TimedSamples>,
}

impl PlayoutQueue {
    /// Queues a packet; timestamps more than a second ahead are treated as bogus and clamped.
    pub fn push(&mut self, timestamp: i64, samples: &[f32], now: i64) {
        let timestamp = timestamp.min(now + MAX_LEAD_HNS);
        self.waiting.push_back(TimedSamples { timestamp, samples: samples.to_vec() });
    }

    /// Removes and returns, oldest first, the packets whose timestamp has been reached.
    pub fn take_due(&mut self, now: i64) -> Vec<TimedSamples> {
        let due = self.waiting.iter().take_while(|packet| packet.timestamp <= now).count();
        self.waiting.drain(..due).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn past_packets_are_due_at_once() {
        let mut queue = PlayoutQueue::default();
        queue.push(90, &[0.5, 0.5], 100);
        let due = queue.take_due(100);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].samples, [0.5, 0.5]);
    }

    #[test]
    fn future_packets_wait_for_their_time() {
        let mut queue = PlayoutQueue::default();
        queue.push(300, &[0.1, 0.1], 100);
        queue.push(400, &[0.2, 0.2], 100);
        assert!(queue.take_due(299).is_empty());
        assert_eq!(queue.take_due(350).iter().map(|p| p.timestamp).collect::<Vec<_>>(), [300]);
        assert_eq!(queue.take_due(400).iter().map(|p| p.timestamp).collect::<Vec<_>>(), [400]);
        assert!(queue.take_due(1_000).is_empty());
    }

    #[test]
    fn bogus_far_future_timestamps_are_clamped() {
        let mut queue = PlayoutQueue::default();
        queue.push(i64::MAX, &[0.0, 0.0], 0);
        assert!(queue.take_due(MAX_LEAD_HNS - 1).is_empty());
        assert_eq!(queue.take_due(MAX_LEAD_HNS)[0].timestamp, MAX_LEAD_HNS);
    }
}
