use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut microphone = neo_voice::Microphone::open(None)?;
    microphone.start()?;
    let mut peak = 0.0_f32;
    for _ in 0..30 {
        let bins = microphone.spectrum();
        assert!(bins.iter().all(|bin| bin.is_finite() && (0.0..=1.0).contains(bin)));
        peak = bins.into_iter().fold(peak, f32::max);
        std::thread::sleep(Duration::from_millis(50));
    }
    let utterance = microphone.stop()?;
    println!("Captured {} samples at {} Hz; FFT peak {peak:.3}; microphone stopped", utterance.pcm16.len(), utterance.sample_rate);
    assert!(!utterance.pcm16.is_empty());
    Ok(())
}
