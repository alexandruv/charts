//! Liquidity: the prices where resting orders are expected to sit.

use super::oscillators;
use super::periods::Reset;
use crate::bars::Bar;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Above,
    Below,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Level {
    pub from: usize,
    pub swept: Option<usize>,
    pub to: Option<usize>,
    pub top: f64,
    pub bottom: f64,
    pub side: Side,
    pub strong: bool,
    pub label: &'static str,
}

impl Level {
    pub fn is_live(&self) -> bool {
        self.to.is_none() && self.swept.is_none()
    }
    pub fn reaches_the_end(&self) -> bool {
        self.to.is_none()
    }
}

const ATR_PERIOD: usize = 14;

fn scale(atr: &[Option<f64>], bars: &[Bar], at: usize) -> f64 {
    atr[at].unwrap_or(bars[at].high - bars[at].low)
}

pub fn liquidity(bars: &[Bar], strength: usize, tolerance: f64, history: bool) -> Vec<Level> {
    let s = strength.max(1);
    if s > bars.len().saturating_sub(1) / 2 {
        return Vec::new();
    }
    let atr = oscillators::atr(bars, ATR_PERIOD);
    let near = |at: usize| tolerance.max(0.0) * scale(&atr, bars, at);

    let mut levels: Vec<Level> = Vec::new();
    let mut highs: Vec<usize> = Vec::new();
    let mut lows: Vec<usize> = Vec::new();

    for (j, bar) in bars.iter().enumerate() {
        if j >= 2 * s {
            let p = j - s;
            if is_swing(bars, p, s, |b| b.high, |a, b| a > b) {
                let price = bars[p].high;
                let strong = highs
                    .last()
                    .is_some_and(|&top| (levels[top].top - price).abs() <= near(p));
                if strong && let Some(&top) = highs.last() {
                    levels[top].strong = true;
                    levels[top].label = "EQH";
                }
                highs.push(levels.len());
                levels.push(line(
                    p,
                    price,
                    Side::Above,
                    strong,
                    if strong { "EQH" } else { "BSL" },
                ));
            }
            if is_swing(bars, p, s, |b| b.low, |a, b| a < b) {
                let price = bars[p].low;
                let strong = lows
                    .last()
                    .is_some_and(|&top| (price - levels[top].bottom).abs() <= near(p));
                if strong && let Some(&top) = lows.last() {
                    levels[top].strong = true;
                    levels[top].label = "EQL";
                }
                lows.push(levels.len());
                levels.push(line(
                    p,
                    price,
                    Side::Below,
                    strong,
                    if strong { "EQL" } else { "SSL" },
                ));
            }
        }

        while let Some(&top) = highs.last() {
            if bar.high <= levels[top].top {
                break;
            }
            levels[top].swept = Some(j);
            levels[top].to = Some(j);
            highs.pop();
        }
        while let Some(&top) = lows.last() {
            if bar.low >= levels[top].bottom {
                break;
            }
            levels[top].swept = Some(j);
            levels[top].to = Some(j);
            lows.pop();
        }
    }

    if !history {
        levels.retain(Level::is_live);
    }
    levels
}

fn is_swing(
    bars: &[Bar],
    p: usize,
    s: usize,
    price: impl Fn(&Bar) -> f64,
    beyond: impl Fn(f64, f64) -> bool,
) -> bool {
    let at = price(&bars[p]);
    bars[p - s..p].iter().all(|b| beyond(at, price(b)))
        && bars[p + 1..=p + s].iter().all(|b| !beyond(price(b), at))
}

fn line(from: usize, price: f64, side: Side, strong: bool, label: &'static str) -> Level {
    Level {
        from,
        swept: None,
        to: None,
        top: price,
        bottom: price,
        side,
        strong,
        label,
    }
}

pub fn key_levels(bars: &[Bar], reset: Reset, session_origin: i64) -> Vec<Level> {
    let (high_label, low_label) = match reset {
        Reset::Session => ("PDH", "PDL"),
        Reset::Week => ("PWH", "PWL"),
        Reset::Month => ("PMH", "PML"),
        Reset::Quarter => ("PQH", "PQL"),
        Reset::Year => ("PYH", "PYL"),
    };

    let mut periods: Vec<(usize, usize, f64, f64)> = Vec::new();
    let mut bucket = None;
    for (i, bar) in bars.iter().enumerate() {
        let this = reset.bucket(bar.ts, session_origin);
        match periods.last_mut() {
            Some(period) if bucket == Some(this) => {
                period.1 = i;
                period.2 = period.2.max(bar.high);
                period.3 = period.3.min(bar.low);
            }
            _ => {
                periods.push((i, i, bar.high, bar.low));
                bucket = Some(this);
            }
        }
    }

    let mut levels = Vec::new();
    for (k, pair) in periods.windows(2).enumerate() {
        let (previous, (start, end, _, _)) = (pair[0], pair[1]);
        let latest = k + 2 == periods.len();
        let to = (!latest).then_some(end);
        let span = &bars[start..=end];
        let (high, low) = (previous.2, previous.3);
        levels.push(Level {
            from: start,
            swept: span.iter().position(|b| b.high > high).map(|at| start + at),
            to,
            top: high,
            bottom: high,
            side: Side::Above,
            strong: false,
            label: high_label,
        });
        levels.push(Level {
            from: start,
            swept: span.iter().position(|b| b.low < low).map(|at| start + at),
            to,
            top: low,
            bottom: low,
            side: Side::Below,
            strong: false,
            label: low_label,
        });
    }
    levels
}

pub fn fair_value_gaps(bars: &[Bar], min_size: f64, history: bool) -> Vec<Level> {
    let atr = oscillators::atr(bars, ATR_PERIOD);
    let mut levels: Vec<Level> = Vec::new();
    let mut open: Vec<usize> = Vec::new();

    for (i, bar) in bars.iter().enumerate() {
        open.retain(|&at| {
            let gap = &mut levels[at];
            let (entered, filled) = match gap.side {
                Side::Below => (bar.low < gap.top, bar.low <= gap.bottom),
                Side::Above => (bar.high > gap.bottom, bar.high >= gap.top),
            };
            if entered && gap.swept.is_none() {
                gap.swept = Some(i);
            }
            if filled {
                gap.to = Some(i);
            }
            !filled
        });

        if i < 2 {
            continue;
        }
        let first = &bars[i - 2];
        let least = min_size.max(0.0) * scale(&atr, bars, i);
        let gap = if bar.low > first.high && bar.low - first.high >= least {
            Some((bar.low, first.high, Side::Below))
        } else if bar.high < first.low && first.low - bar.high >= least {
            Some((first.low, bar.high, Side::Above))
        } else {
            None
        };
        if let Some((top, bottom, side)) = gap {
            open.push(levels.len());
            levels.push(Level {
                from: i - 1,
                swept: None,
                to: None,
                top,
                bottom,
                side,
                strong: false,
                label: "",
            });
        }
    }

    if !history {
        levels.retain(|level| level.to.is_none());
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    /// Bars from (high, low) pairs, an hour apart.
    fn bars(ranges: &[(f64, f64)]) -> Vec<Bar> {
        ranges
            .iter()
            .enumerate()
            .map(|(i, &(high, low))| Bar {
                ts: i as i64 * 3_600,
                open: (high + low) / 2.0,
                high,
                low,
                close: (high + low) / 2.0,
                volume: 1.0,
            })
            .collect()
    }

    /// A peak at bar 2, then lower bars: one swing high, still live.
    #[test]
    fn a_swing_high_is_buy_side_liquidity_until_something_trades_above_it() {
        let series = bars(&[
            (10.0, 9.0),
            (11.0, 10.0),
            (12.0, 11.0),
            (11.0, 10.0),
            (10.5, 9.5),
        ]);
        let found = liquidity(&series, 2, 0.0, true);
        let highs: Vec<&Level> = found.iter().filter(|l| l.side == Side::Above).collect();
        assert_eq!(highs.len(), 1);
        assert_eq!((highs[0].from, highs[0].top), (2, 12.0));
        assert!(highs[0].is_live());
        assert_eq!(highs[0].label, "BSL");
    }

    #[test]
    fn a_bar_through_the_high_sweeps_it_and_ends_the_line_there() {
        let series = bars(&[
            (10.0, 9.0),
            (11.0, 10.0),
            (12.0, 11.0),
            (11.0, 10.0),
            (10.5, 9.5),
            (11.5, 10.5),
            (12.5, 11.0), // through 12.0
        ]);
        let high = liquidity(&series, 2, 0.0, true)
            .into_iter()
            .find(|l| l.side == Side::Above && l.top == 12.0)
            .expect("the swing high");
        assert_eq!(high.swept, Some(6));
        assert_eq!(high.to, Some(6));

        let live_only = liquidity(&series, 2, 0.0, false);
        assert!(
            live_only.iter().all(|l| l.top != 12.0),
            "with history off a swept level is not drawn"
        );
    }

    #[test]
    fn touching_a_level_exactly_does_not_sweep_it() {
        let series = bars(&[
            (10.0, 9.0),
            (11.0, 10.0),
            (12.0, 11.0),
            (11.0, 10.0),
            (10.5, 9.5),
            (12.0, 11.0),
        ]);
        let high = liquidity(&series, 2, 0.0, true)
            .into_iter()
            .find(|l| l.side == Side::Above && l.from == 2)
            .unwrap();
        assert!(high.is_live());
    }

    #[test]
    fn two_highs_at_the_same_price_are_equal_highs() {
        let series = bars(&[
            (10.0, 9.0),
            (11.0, 10.0),
            (12.0, 11.0),
            (11.0, 10.0),
            (10.0, 9.0),
            (11.0, 10.0),
            (12.0, 11.0),
            (11.0, 10.0),
            (10.0, 9.0),
        ]);
        let highs: Vec<Level> = liquidity(&series, 2, 0.1, true)
            .into_iter()
            .filter(|l| l.side == Side::Above)
            .collect();
        assert_eq!(highs.len(), 2, "{highs:?}");
        assert!(
            highs
                .iter()
                .all(|l| l.strong && l.label == "EQH" && l.is_live())
        );
    }

    #[test]
    fn swing_lows_mirror_swing_highs() {
        let series = bars(&[
            (12.0, 11.0),
            (11.0, 10.0),
            (10.0, 9.0),
            (11.0, 10.0),
            (12.0, 10.5),
            (11.0, 8.5),
        ]);
        let low = liquidity(&series, 2, 0.0, true)
            .into_iter()
            .find(|l| l.side == Side::Below)
            .expect("a swing low");
        assert_eq!((low.from, low.bottom, low.swept), (2, 9.0, Some(5)));
        assert_eq!(low.label, "SSL");
    }

    #[test]
    fn a_short_series_has_no_swings_and_no_panic() {
        assert!(liquidity(&[], 5, 0.1, true).is_empty());
        assert!(liquidity(&bars(&[(1.0, 0.5); 4]), usize::MAX, 0.1, true).is_empty());
        assert!(liquidity(&bars(&[(1.0, 0.5); 4]), 5, 0.1, true).is_empty());
        assert!(fair_value_gaps(&bars(&[(1.0, 0.5)]), 0.0, true).is_empty());
        assert!(key_levels(&[], Reset::Session, 0).is_empty());
    }

    /// Two days of hourly bars; the second day runs above the first's high.
    #[test]
    fn the_previous_days_high_and_low_are_carried_across_the_next_day() {
        let mut series = Vec::new();
        for hour in 0..24 {
            let (high, low) = if hour == 5 {
                (110.0, 100.0)
            } else {
                (105.0, 101.0)
            };
            series.push(Bar {
                ts: hour * 3_600,
                open: 103.0,
                high,
                low,
                close: 103.0,
                volume: 1.0,
            });
        }
        for hour in 0..24 {
            let high = if hour == 10 { 111.0 } else { 106.0 };
            series.push(Bar {
                ts: DAY + hour * 3_600,
                open: 104.0,
                high,
                low: 102.0,
                close: 104.0,
                volume: 1.0,
            });
        }
        let levels = key_levels(&series, Reset::Session, 0);
        assert_eq!(levels.len(), 2);
        let (high, low) = (&levels[0], &levels[1]);
        assert_eq!((high.top, high.label, high.from), (110.0, "PDH", 24));
        assert_eq!(high.swept, Some(34), "taken at hour ten of the second day");
        assert_eq!(high.to, None, "the latest day's levels run to the edge");
        assert_eq!((low.bottom, low.label, low.swept), (100.0, "PDL", None));
    }

    #[test]
    fn a_bullish_gap_is_support_until_price_trades_back_through_it() {
        let series = bars(&[
            (10.0, 9.0),
            (13.0, 10.0),
            (14.0, 12.0), // low 12 above the first bar's high 10: a gap 10-12
            (15.0, 13.0),
            (14.0, 11.0), // enters the gap
            (13.0, 9.5),  // fills it
        ]);
        let gaps = fair_value_gaps(&series, 0.0, true);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        let gap = &gaps[0];
        assert_eq!(
            (gap.side, gap.top, gap.bottom, gap.from),
            (Side::Below, 12.0, 10.0, 1)
        );
        assert_eq!((gap.swept, gap.to), (Some(4), Some(5)));
        assert!(
            fair_value_gaps(&series, 0.0, false).is_empty(),
            "filled, so gone without history"
        );
    }

    #[test]
    fn a_bearish_gap_is_resistance_overhead() {
        let series = bars(&[(20.0, 19.0), (19.0, 16.0), (17.0, 15.0), (16.0, 14.0)]);
        let gaps = fair_value_gaps(&series, 0.0, true);
        assert_eq!(gaps.len(), 1);
        assert_eq!(
            (gaps[0].side, gaps[0].top, gaps[0].bottom),
            (Side::Above, 19.0, 17.0)
        );
        assert!(gaps[0].is_live());
    }

    #[test]
    fn a_gap_smaller_than_the_minimum_is_left_out() {
        let series = bars(&[(10.0, 9.0), (10.5, 9.8), (10.6, 10.01), (10.7, 10.2)]);
        assert_eq!(fair_value_gaps(&series, 0.0, true).len(), 1);
        assert!(fair_value_gaps(&series, 1.0, true).is_empty());
    }

    /// A long trend leaves many levels live without losing their history.
    #[test]
    fn a_long_trend_keeps_its_levels() {
        let series: Vec<Bar> = (0..12_000)
            .map(|i| {
                let base = i as f64 * 0.1 + (i as f64 / 3.0).sin() * 2.0;
                Bar {
                    ts: i * 3_600,
                    open: base,
                    high: base + 1.0,
                    low: base - 1.0,
                    close: base,
                    volume: 1.0,
                }
            })
            .collect();

        let found = liquidity(&series, 2, 0.1, true);
        let gaps = fair_value_gaps(&series, 0.0, true);
        assert!(!found.is_empty());
        let _ = gaps;
    }
}
