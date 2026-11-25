//! How much has vested at a time. Pure, total, rounds down.

use borsh::{BorshDeserialize, BorshSerialize};

use crate::{Timestamp, MAX_TRANCHES};

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Shape {
    Linear {
        start: Timestamp,
        end: Timestamp,
    },
    /// Nothing before `cliff`; from then on, what a linear schedule would have accrued since
    /// `start`.
    CliffLinear {
        start: Timestamp,
        cliff: Timestamp,
        end: Timestamp,
    },
    Milestones {
        tranches: Vec<u128>,
        /// Bit `i` is tranche `i`.
        signalled: u64,
    },
}

impl Shape {
    /// Whether a new schedule of `total` may have this shape.
    #[must_use]
    pub fn is_valid(&self, total: u128) -> bool {
        match self {
            Self::Linear { start, end } => start < end,
            Self::CliffLinear { start, cliff, end } => {
                start < end && start <= cliff && cliff <= end
            }
            Self::Milestones {
                tranches,
                signalled,
            } => {
                *signalled == 0
                    && !tranches.is_empty()
                    && tranches.len() <= MAX_TRANCHES
                    && tranches.iter().all(|amount| *amount > 0)
                    && tranches
                        .iter()
                        .try_fold(0u128, |sum, amount| sum.checked_add(*amount))
                        == Some(total)
            }
        }
    }

    #[must_use]
    pub const fn is_time_based(&self) -> bool {
        !matches!(self, Self::Milestones { .. })
    }

    /// At most `total`, never decreasing in `at`, exactly `total` from the end on.
    #[must_use]
    pub fn vested(&self, total: u128, at: Timestamp) -> u128 {
        match self {
            Self::Linear { start, end } => linear(total, *start, *end, at),
            Self::CliffLinear { start, cliff, end } => {
                if at < *cliff {
                    0
                } else {
                    linear(total, *start, *end, at)
                }
            }
            Self::Milestones {
                tranches,
                signalled,
            } => tranches
                .iter()
                .enumerate()
                .filter(|(index, _)| signalled & (1u64 << index) != 0)
                .map(|(_, amount)| *amount)
                .sum(),
        }
    }
}

/// `floor(total * (at - start) / (end - start))`, clamped. `total = q * span + r` keeps every
/// product inside u128: `q * elapsed <= total` and `r, elapsed < 2^64`.
fn linear(total: u128, start: Timestamp, end: Timestamp, at: Timestamp) -> u128 {
    if at <= start {
        return 0;
    }
    if at >= end {
        return total;
    }
    let span = u128::from(end - start);
    let elapsed = u128::from(at - start);
    (total / span) * elapsed + (total % span) * elapsed / span
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn shapes() -> impl Strategy<Value = Shape> {
        let times = (any::<u64>(), any::<u64>(), any::<u64>())
            .prop_map(|(a, b, c)| {
                let mut t = [a, b, c];
                t.sort_unstable();
                t
            })
            .prop_filter("a positive span", |t| t[0] < t[2]);
        prop_oneof![
            times
                .clone()
                .prop_map(|[start, _, end]| Shape::Linear { start, end }),
            times.prop_map(|[start, cliff, end]| Shape::CliffLinear { start, cliff, end }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20_000))]

        #[test]
        fn vested_is_bounded_monotone_and_complete(
            shape in shapes(), total in any::<u128>(), a in any::<u64>(), b in any::<u64>()
        ) {
            prop_assume!(total > 0);
            prop_assert!(shape.is_valid(total));
            let (early, late) = (a.min(b), a.max(b));
            prop_assert!(shape.vested(total, early) <= shape.vested(total, late));
            prop_assert!(shape.vested(total, late) <= total);
            prop_assert_eq!(shape.vested(total, u64::MAX), total);
            prop_assert_eq!(shape.vested(total, 0), 0);
        }

        /// Against the direct formula, where it fits.
        #[test]
        fn linear_matches_the_direct_formula(
            total in 0u128..1 << 64, start in 0u64..1 << 62, span in 1u64..1 << 62, part in any::<u64>()
        ) {
            let elapsed = part % (span + 1);
            let direct = total * u128::from(elapsed) / u128::from(span);
            prop_assert_eq!(linear(total, start, start + span, start + elapsed), direct);
        }
    }

    #[test]
    fn a_cliff_releases_what_accrued_since_the_start_then_continues_linearly() {
        let shape = Shape::CliffLinear {
            start: 1_000,
            cliff: 1_250,
            end: 2_000,
        };
        for (at, expected) in [
            (1_249, 0),
            (1_250, 250),
            (1_251, 251),
            (1_999, 999),
            (2_000, 1_000),
        ] {
            assert_eq!(shape.vested(1_000, at), expected, "at {at}");
        }
    }

    #[test]
    fn a_cliff_may_fall_on_the_start_or_the_end() {
        for cliff in [5, 9] {
            let shape = Shape::CliffLinear {
                start: 5,
                cliff,
                end: 9,
            };
            assert!(shape.is_valid(100), "{shape:?}");
        }
    }

    #[test]
    fn malformed_shapes_are_refused() {
        let bad = [
            Shape::Linear { start: 5, end: 5 },
            Shape::CliffLinear {
                start: 5,
                cliff: 4,
                end: 9,
            },
            Shape::CliffLinear {
                start: 5,
                cliff: 10,
                end: 9,
            },
            Shape::Milestones {
                tranches: vec![],
                signalled: 0,
            },
            Shape::Milestones {
                tranches: vec![60, 41],
                signalled: 0,
            },
            Shape::Milestones {
                tranches: vec![100, 0],
                signalled: 0,
            },
            Shape::Milestones {
                tranches: vec![100],
                signalled: 1,
            },
        ];
        for shape in bad {
            assert!(!shape.is_valid(100), "{shape:?}");
        }
    }
}
