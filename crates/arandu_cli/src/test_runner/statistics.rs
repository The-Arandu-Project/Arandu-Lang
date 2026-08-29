//! Pure benchmark sample statistics and presentation units.

use arandu_codegen::testing::BenchmarkEventV1;

pub fn sample_values(event: &BenchmarkEventV1) -> Vec<f64> {
    let mut values = event
        .samples
        .iter()
        .filter(|sample| sample.iterations != 0)
        .map(|sample| sample.elapsed_ns as f64 / sample.iterations as f64)
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    values
}

pub fn percentile(sorted: &[f64], percentile: usize) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let numerator = (sorted.len() - 1).checked_mul(percentile)?;
    sorted.get(numerator.div_ceil(100)).copied()
}

pub fn median(sorted: &[f64]) -> Option<f64> {
    let middle = sorted.len().checked_div(2)?;
    if sorted.len().is_multiple_of(2) {
        let left = *sorted.get(middle.checked_sub(1)?)?;
        let right = *sorted.get(middle)?;
        Some((left + right) / 2.0)
    } else {
        sorted.get(middle).copied()
    }
}

pub fn benchmark_stats(event: &BenchmarkEventV1) -> (Option<f64>, Option<f64>, Option<f64>) {
    let values = sample_values(event);
    let center_median = median(&values);
    let mad = center_median.and_then(|center| {
        let mut deviations = values
            .iter()
            .map(|value| (value - center).abs())
            .collect::<Vec<_>>();
        deviations.sort_by(f64::total_cmp);
        median(&deviations)
    });
    (center_median, mad, percentile(&values, 95))
}

pub fn format_ns_per_op(value: f64) -> String {
    if value >= 1_000_000.0 {
        format!("{:.3} ms/op", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.3} us/op", value / 1_000.0)
    } else {
        format!("{value:.3} ns/op")
    }
}
