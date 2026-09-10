//! Which toolbar groups fit, and which go into an overflow menu.

use super::EXTENT;

/// Most groups [`fit_toolbar`] accepts.
const MAX_GROUPS: usize = 4096;

/// The outcome of [`fit_toolbar`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolbarFit {
    /// Groups to draw, in order, each with its natural width.
    pub shown: Vec<(usize, f32)>,
    /// Groups that belong in the overflow menu, in order.
    pub overflow: Vec<usize>,
    /// Width to reserve for the overflow control (0 when nothing overflows).
    pub overflow_width: f32,
}

fn length_ok(v: f32) -> bool {
    v.is_finite() && (0.0..=EXTENT).contains(&v)
}

/// Fit toolbar groups of `natural` widths, `spacing` apart, into `available` points.
///
/// When everything fits, every group is shown. Otherwise a leading run of groups is shown at its
/// natural width, followed by an overflow control `overflow_width` wide that lists the rest:
/// groups are never shrunk, reordered or skipped. Returns `None` for malformed input and when not
/// even the overflow control fits, so the caller has to pick its own fallback.
pub fn fit_toolbar(natural: &[f32], available: f32, spacing: f32, overflow_width: f32) -> Option<ToolbarFit> {
    if natural.len() > MAX_GROUPS || !length_ok(available) || !length_ok(spacing) || !length_ok(overflow_width) || overflow_width == 0.0 {
        return None;
    }
    if natural.iter().any(|&w| !w.is_finite() || w <= 0.0 || w > EXTENT) {
        return None;
    }
    if natural.is_empty() {
        return Some(ToolbarFit::default());
    }
    let gaps = f64::from(spacing) * (natural.len() - 1) as f64;
    let total: f64 = natural.iter().map(|&w| f64::from(w)).sum::<f64>() + gaps;
    if total <= f64::from(available) {
        return Some(ToolbarFit { shown: natural.iter().copied().enumerate().collect(), overflow: Vec::new(), overflow_width: 0.0 });
    }
    if available < overflow_width {
        return None;
    }
    let budget = f64::from(available) - f64::from(overflow_width);
    let mut used = 0.0f64;
    let mut count = 0;
    for &w in natural {
        // Each shown group also pays for the gap after it (to the next group or the control).
        let next = used + f64::from(w) + f64::from(spacing);
        if next > budget {
            break;
        }
        used = next;
        count += 1;
    }
    Some(ToolbarFit { shown: natural.iter().copied().enumerate().take(count).collect(), overflow: (count..natural.len()).collect(), overflow_width })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(fit: &ToolbarFit) -> Vec<usize> {
        fit.shown.iter().map(|&(i, _)| i).collect()
    }

    #[test]
    fn everything_fits_or_nothing_to_fit() {
        let fit = fit_toolbar(&[24.0, 36.0, 48.0], 120.0, 6.0, 20.0).unwrap();
        assert_eq!(fit.shown, vec![(0, 24.0), (1, 36.0), (2, 48.0)]);
        assert!(fit.overflow.is_empty());
        assert_eq!(fit.overflow_width, 0.0);
        assert_eq!(fit_toolbar(&[], 50.0, 4.0, 20.0), Some(ToolbarFit::default()));
        assert_eq!(shown(&fit_toolbar(&[10.0], 10.0, 4.0, 20.0).unwrap()), vec![0]);
    }

    #[test]
    fn a_prefix_stays_and_the_rest_overflows() {
        let fit = fit_toolbar(&[24.0, 36.0, 48.0], 96.0, 4.0, 20.0).unwrap();
        assert_eq!((shown(&fit), fit.overflow.clone(), fit.overflow_width), (vec![0, 1], vec![2], 20.0));
        let fit = fit_toolbar(&[24.0, 36.0, 48.0], 87.0, 4.0, 20.0).unwrap();
        assert_eq!((shown(&fit), fit.overflow), (vec![0], vec![1, 2]));
        // A wide first group is not skipped to show narrower ones after it.
        let fit = fit_toolbar(&[200.0, 24.0, 24.0], 120.0, 4.0, 20.0).unwrap();
        assert_eq!((shown(&fit), fit.overflow), (vec![], vec![0, 1, 2]));
        let fit = fit_toolbar(&[200.0], 60.0, 4.0, 20.0).unwrap();
        assert_eq!(fit.overflow, vec![0]);
    }

    #[test]
    fn too_narrow_for_the_overflow_control() {
        for available in [0.0, 1.0, 19.999] {
            assert_eq!(fit_toolbar(&[30.0, 30.0], available, 4.0, 20.0), None, "{available}");
        }
        let fit = fit_toolbar(&[30.0, 30.0], 20.0, 4.0, 20.0).unwrap();
        assert_eq!((shown(&fit), fit.overflow), (vec![], vec![0, 1]));
    }

    #[test]
    fn malformed_input_is_rejected() {
        let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, EXTENT + 1.0, f32::MAX];
        for b in bad {
            assert_eq!(fit_toolbar(&[10.0, b], 100.0, 4.0, 20.0), None, "width {b}");
            assert_eq!(fit_toolbar(&[10.0], b, 4.0, 20.0), None, "available {b}");
            assert_eq!(fit_toolbar(&[10.0], 100.0, b, 20.0), None, "spacing {b}");
            assert_eq!(fit_toolbar(&[10.0], 100.0, 4.0, b), None, "overflow {b}");
        }
        assert_eq!(fit_toolbar(&[0.0], 100.0, 4.0, 20.0), None);
        assert_eq!(fit_toolbar(&[10.0], 100.0, 4.0, 0.0), None);
        assert!(fit_toolbar(&vec![1.0; MAX_GROUPS], 10.0, 0.0, 5.0).is_some());
        assert_eq!(fit_toolbar(&vec![1.0; MAX_GROUPS + 1], 10.0, 0.0, 5.0), None);
    }

    #[test]
    fn groups_stay_whole_ordered_and_inside() {
        let widths = [12.5, 28.0, 24.25, 58.0, 32.0];
        for spacing in [0.0, 1.0, 3.5, 8.0] {
            for step in 0..=400 {
                let available = step as f32 * 0.5;
                let Some(fit) = fit_toolbar(&widths, available, spacing, 18.0) else { continue };
                let mut all = shown(&fit);
                all.extend(&fit.overflow);
                assert_eq!(all, vec![0, 1, 2, 3, 4], "{available} {spacing}");
                for &(i, w) in &fit.shown {
                    assert_eq!(w.to_bits(), widths[i].to_bits());
                }
                let count = fit.shown.len() + usize::from(fit.overflow_width > 0.0);
                let gaps = spacing * count.saturating_sub(1) as f32;
                let used: f32 = fit.shown.iter().map(|&(_, w)| w).sum::<f32>() + fit.overflow_width + gaps;
                assert!(used <= available + 1e-3, "{used} > {available} ({spacing})");
            }
        }
    }
}
