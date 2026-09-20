//! Mean-pool embedding windows and L2-normalize the result.

use anyhow::{Result, bail};

/// Average window vectors, then L2-normalize. Windows may have different
/// pre-pool norms; the last window may be shorter than the rest.
pub fn mean_pool_normalized_windows(windows: &[Vec<f32>]) -> Result<Vec<f32>> {
    let Some(first) = windows.first() else {
        bail!("no embedding windows");
    };
    let dim = first.len();
    if dim == 0 {
        bail!("model reported zero embedding dimensions");
    }

    let mut accumulator = vec![0.0_f32; dim];
    for window in windows {
        if window.len() != dim {
            bail!("inconsistent embedding dimensions across windows");
        }
        for (accumulated, value) in accumulator.iter_mut().zip(window) {
            *accumulated += *value;
        }
    }
    let divisor = windows.len() as f32;
    for value in &mut accumulator {
        *value /= divisor;
    }

    let mut norm_sq = 0.0_f32;
    for value in &accumulator {
        norm_sq += *value * *value;
    }
    let norm = norm_sq.sqrt();
    if norm == 0.0 {
        bail!("pooled embedding has zero L2 norm");
    }
    for value in &mut accumulator {
        *value /= norm;
    }
    Ok(accumulator)
}

#[cfg(test)]
mod tests {
    use super::mean_pool_normalized_windows;

    fn l2(values: &[f32]) -> f32 {
        values.iter().map(|value| value * value).sum::<f32>().sqrt()
    }

    #[test]
    fn mean_pool_includes_short_last_window_and_unit_normalizes() {
        let first = vec![4.0, 0.0];
        let last = vec![0.0, 2.0];
        let pooled = mean_pool_normalized_windows(&[first, last])
            .unwrap_or_else(|error| panic!("pool failed: {error}"));

        assert_eq!(pooled.len(), 2);
        assert!((l2(&pooled) - 1.0).abs() < 1e-6);
        let scale = 5.0_f32.sqrt();
        assert!((pooled[0] - 2.0 / scale).abs() < 1e-6);
        assert!((pooled[1] - 1.0 / scale).abs() < 1e-6);
    }

    #[test]
    fn mean_pool_rejects_empty_and_zero_norm() {
        assert!(mean_pool_normalized_windows(&[]).is_err());
        assert!(mean_pool_normalized_windows(&[vec![0.0, 0.0]]).is_err());
    }

    #[test]
    fn mean_pool_rejects_dimension_mismatch() {
        assert!(mean_pool_normalized_windows(&[vec![1.0, 0.0], vec![1.0]]).is_err());
    }
}
