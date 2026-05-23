use crossbeam_channel::Sender;

pub(crate) fn write_f32(
    data: &[f32],
    channels: usize,
    data_tx: &Sender<Vec<i16>>,
    pool_rx: &crossbeam_channel::Receiver<Vec<i16>>,
) {
    write_interleaved_mono(data, channels, data_tx, pool_rx, |sample| {
        (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
    });
}

pub(crate) fn write_i16(
    data: &[i16],
    channels: usize,
    data_tx: &Sender<Vec<i16>>,
    pool_rx: &crossbeam_channel::Receiver<Vec<i16>>,
) {
    write_interleaved_mono(data, channels, data_tx, pool_rx, |sample| *sample);
}

pub(crate) fn write_u16(
    data: &[u16],
    channels: usize,
    data_tx: &Sender<Vec<i16>>,
    pool_rx: &crossbeam_channel::Receiver<Vec<i16>>,
) {
    write_interleaved_mono(data, channels, data_tx, pool_rx, |sample| {
        (*sample as i32 - 32768) as i16
    });
}

fn write_interleaved_mono<T>(
    data: &[T],
    channels: usize,
    data_tx: &Sender<Vec<i16>>,
    pool_rx: &crossbeam_channel::Receiver<Vec<i16>>,
    to_i16: impl Fn(&T) -> i16,
) {
    let channels = channels.max(1);

    // Reuse a pooled buffer, or allocate if the pool is empty.
    let mut chunk = pool_rx
        .try_recv()
        .unwrap_or_else(|_| Vec::with_capacity(data.len() / channels));

    if channels == 1 {
        chunk.extend(data.iter().map(to_i16));
    } else {
        chunk.extend(data.chunks(channels).map(|frame| {
            let sum = frame
                .iter()
                .map(|sample| to_i16(sample) as i32)
                .sum::<i32>();
            (sum / frame.len().max(1) as i32) as i16
        }));
    }

    if let Err(crossbeam_channel::TrySendError::Full(_)) = data_tx.try_send(chunk) {
        tracing::warn!(
            "audio data channel full ({} chunks); dropping chunk to keep callback real-time",
            data_tx.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel;

    fn drain_all(receiver: &crossbeam_channel::Receiver<Vec<i16>>) -> Vec<i16> {
        let mut all = Vec::new();
        while let Ok(chunk) = receiver.try_recv() {
            all.extend(chunk);
        }
        all
    }

    #[test]
    fn write_i16_mono_roundtrip() {
        let (data_tx, data_rx) = crossbeam_channel::bounded(8);
        let (_, pool_rx) = crossbeam_channel::bounded(8);
        let data = [1000_i16, -500, 2000, -1000];
        write_i16(&data, 1, &data_tx, &pool_rx);
        drop(data_tx);
        assert_eq!(drain_all(&data_rx), vec![1000, -500, 2000, -1000]);
    }

    #[test]
    fn write_i16_stereo_averages_channels() {
        let (data_tx, data_rx) = crossbeam_channel::bounded(8);
        let (_, pool_rx) = crossbeam_channel::bounded(8);
        let data = [1000_i16, 2000, -500, 1500];
        write_i16(&data, 2, &data_tx, &pool_rx);
        drop(data_tx);
        // Averaged: (1000+2000)/2 = 1500, (-500+1500)/2 = 500
        assert_eq!(drain_all(&data_rx), vec![1500, 500]);
    }

    #[test]
    fn write_f32_clamps_and_converts() {
        let (data_tx, data_rx) = crossbeam_channel::bounded(8);
        let (_, pool_rx) = crossbeam_channel::bounded(8);
        let data = [0.5_f32, -0.5, 2.0, -2.0];
        write_f32(&data, 1, &data_tx, &pool_rx);
        drop(data_tx);
        let samples = drain_all(&data_rx);
        let expected_05 = (0.5 * i16::MAX as f32) as i16;
        let expected_max = i16::MAX;
        assert_eq!(
            samples,
            vec![expected_05, -expected_05, expected_max, -32767]
        );
    }

    #[test]
    fn write_u16_offsets_correctly() {
        let (data_tx, data_rx) = crossbeam_channel::bounded(8);
        let (_, pool_rx) = crossbeam_channel::bounded(8);
        let data = [32768_u16, 32767, 32769, 0];
        write_u16(&data, 1, &data_tx, &pool_rx);
        drop(data_tx);
        assert_eq!(drain_all(&data_rx), vec![0, -1, 1, -32768]);
    }
}
