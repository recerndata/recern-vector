use std::fmt;
use std::str::FromStr;

/// Distance function of a collection. For every metric, smaller is closer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Metric {
    /// `1 - cos(a, b)`. Vectors are normalized when they are stored.
    Cosine,
    /// Squared Euclidean distance.
    L2,
    /// Negative inner product.
    Dot,
}

impl Metric {
    pub fn as_str(self) -> &'static str {
        match self {
            Metric::Cosine => "cosine",
            Metric::L2 => "l2",
            Metric::Dot => "dot",
        }
    }

    #[inline]
    pub fn distance(self, a: &[f32], b: &[f32]) -> f32 {
        match self {
            Metric::Cosine => 1.0 - dot(a, b),
            Metric::L2 => l2_squared(a, b),
            Metric::Dot => -dot(a, b),
        }
    }

    pub(crate) fn code(self) -> u8 {
        match self {
            Metric::Cosine => 0,
            Metric::L2 => 1,
            Metric::Dot => 2,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Metric::Cosine),
            1 => Some(Metric::L2),
            2 => Some(Metric::Dot),
            _ => None,
        }
    }
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Metric {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "cosine" => Ok(Metric::Cosine),
            "l2" | "euclidean" => Ok(Metric::L2),
            "dot" | "ip" | "inner_product" => Ok(Metric::Dot),
            other => Err(format!(
                "unknown metric '{other}' (expected cosine, l2 or dot)"
            )),
        }
    }
}

// Independent accumulators let the compiler vectorize the loops without
// relaxing floating-point ordering.
const LANES: usize = 8;

#[inline]
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let (chunks_a, chunks_b) = (a.chunks_exact(LANES), b.chunks_exact(LANES));
    let (rest_a, rest_b) = (chunks_a.remainder(), chunks_b.remainder());
    let mut acc = [0.0f32; LANES];
    for (x, y) in chunks_a.zip(chunks_b) {
        for i in 0..LANES {
            acc[i] += x[i] * y[i];
        }
    }
    let mut sum: f32 = acc.iter().sum();
    for (x, y) in rest_a.iter().zip(rest_b) {
        sum += x * y;
    }
    sum
}

#[inline]
pub(crate) fn l2_squared(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let (chunks_a, chunks_b) = (a.chunks_exact(LANES), b.chunks_exact(LANES));
    let (rest_a, rest_b) = (chunks_a.remainder(), chunks_b.remainder());
    let mut acc = [0.0f32; LANES];
    for (x, y) in chunks_a.zip(chunks_b) {
        for i in 0..LANES {
            let d = x[i] - y[i];
            acc[i] += d * d;
        }
    }
    let mut sum: f32 = acc.iter().sum();
    for (x, y) in rest_a.iter().zip(rest_b) {
        let d = x - y;
        sum += d * d;
    }
    sum
}

/// Scales `v` to unit length. Returns `false` for a zero vector.
pub(crate) fn normalize(v: &mut [f32]) -> bool {
    let norm = dot(v, v).sqrt();
    if norm == 0.0 || !norm.is_finite() {
        return false;
    }
    for x in v {
        *x /= norm;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_match_naive_implementation() {
        let a: Vec<f32> = (0..19).map(|i| i as f32 * 0.5 - 3.0).collect();
        let b: Vec<f32> = (0..19).map(|i| (i as f32).sin()).collect();
        let naive_dot: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        let naive_l2: f32 = a.iter().zip(&b).map(|(x, y)| (x - y) * (x - y)).sum();
        assert!((dot(&a, &b) - naive_dot).abs() < 1e-4);
        assert!((l2_squared(&a, &b) - naive_l2).abs() < 1e-3);
    }

    #[test]
    fn normalize_rejects_zero_vector() {
        let mut v = [3.0, 4.0];
        assert!(normalize(&mut v));
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert!(!normalize(&mut [0.0, 0.0]));
    }

    #[test]
    fn parses_metric_names() {
        assert_eq!("Cosine".parse::<Metric>(), Ok(Metric::Cosine));
        assert_eq!("euclidean".parse::<Metric>(), Ok(Metric::L2));
        assert_eq!("ip".parse::<Metric>(), Ok(Metric::Dot));
        assert!("manhattan".parse::<Metric>().is_err());
    }
}
