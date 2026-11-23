use super::{Arg, CriteriaKey, FValue, RangeKey, functions::Crit};
use std::collections::VecDeque;

// A single numeric inequality can be answered from two cut points in a sorted column.
// Only exactly summable integer amounts may change summation order.
struct Sorted {
    keys: Vec<f64>,
    prefix: Option<Vec<i64>>,
    total: i64,
}

impl Sorted {
    fn build(keys: &[FValue], amounts: Option<&[FValue]>) -> Option<Self> {
        const EXACT: u64 = 1 << 53;
        let n = keys.len().min(amounts.map_or(usize::MAX, |v| v.len()));
        let mut rows = Vec::with_capacity(n);
        let mut total = 0i64;
        let mut absolute = 0u64;
        for (i, key) in keys[..n].iter().enumerate() {
            let amount = match amounts {
                None => 1,
                Some(v) => {
                    let x = v[i].to_number().unwrap_or(0.0);
                    if x.fract() != 0.0 || x.abs() > EXACT as f64 {
                        return None;
                    }
                    let weight = x as i64;
                    absolute += weight.unsigned_abs();
                    if absolute > EXACT {
                        return None;
                    }
                    weight
                }
            };
            total += amount;
            if let Ok(value) = key.to_number() {
                if !value.is_finite() {
                    return None;
                }
                rows.push((value, amount));
            }
        }
        rows.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut prefix = amounts.map(|_| vec![0i64]);
        let keys = rows
            .into_iter()
            .map(|(value, amount)| {
                if let Some(p) = &mut prefix {
                    p.push(p.last().copied().unwrap_or(0) + amount);
                }
                value
            })
            .collect();
        Some(Self {
            keys,
            prefix,
            total,
        })
    }

    fn query(&self, op: &str, n: f64) -> FValue {
        let at = |pos: usize| self.prefix.as_ref().map_or(pos as i64, |p| p[pos]);
        let numeric = at(self.keys.len());
        let sum = match op {
            "<" => at(self.keys.partition_point(|x| *x < n)),
            "<=" => at(self.keys.partition_point(|x| *x <= n)),
            ">" => numeric - at(self.keys.partition_point(|x| *x <= n)),
            ">=" => numeric - at(self.keys.partition_point(|x| *x < n)),
            "<>" => {
                // A failed numeric coercion matches <>. Equality uses an epsilon
                // in the scan, so subtract precisely its interval from all rows.
                let lo = self.keys.partition_point(|x| x - n < -f64::EPSILON);
                let hi = self.keys.partition_point(|x| x - n <= f64::EPSILON);
                self.total - (at(hi) - at(lo))
            }
            _ => unreachable!(),
        };
        FValue::number(sum as f64)
    }
}

struct Entry {
    ranges: Vec<RangeKey>,
    attempted: bool,
    sorted: Option<Sorted>,
}

#[derive(Default)]
pub(super) struct OrderedCriteria(VecDeque<Entry>);

impl OrderedCriteria {
    pub(super) fn lookup(&mut self, key: &CriteriaKey, args: &[Arg]) -> Option<FValue> {
        const LIMIT: usize = 262_144;
        if key.function > 1 || key.criteria.len() != 1 {
            return None;
        }
        let sum = key.function == 0;
        if args.len() != if sum { 3 } else { 2 } {
            return None;
        }
        let input = args.get(usize::from(sum))?.slice();
        let (op, number) = Crit::parse(&key.criteria[0]).numeric_bounds()?;
        if !(256..=LIMIT).contains(&input.len()) {
            return None;
        }
        let Some(i) = self.0.iter().position(|e| e.ranges == key.ranges) else {
            if self.0.len() == 4 {
                self.0.pop_front();
            }
            self.0.push_back(Entry {
                ranges: key.ranges.clone(),
                attempted: false,
                sorted: None,
            });
            return None;
        };
        let mut entry = self.0.remove(i)?;
        if !entry.attempted {
            entry.attempted = true;
            while self
                .0
                .iter()
                .filter_map(|e| e.sorted.as_ref())
                .map(|s| s.keys.len())
                .sum::<usize>()
                + input.len()
                > LIMIT
            {
                self.0.pop_front();
            }
            entry.sorted = Sorted::build(input, sum.then(|| args[0].slice()));
        }
        let answer = entry.sorted.as_ref().map(|s| s.query(op, number));
        self.0.push_back(entry);
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inequalities_match_scan_coercions_boundaries_and_exact_sum_order() {
        let data = [
            FValue::Empty,
            FValue::Bool(true),
            FValue::Text(" 1 ".into()),
            FValue::Error(super::super::ErrKind::Value),
            FValue::Text("abc".into()),
            FValue::Number(-0.0),
            FValue::Number(1.0 + f64::EPSILON),
            FValue::Number(2.5),
        ];
        let amounts: Vec<_> = (0..data.len())
            .map(|i| FValue::Number(i as f64 - 3.))
            .collect();
        for weights in [None, Some(amounts.as_slice())] {
            let sorted = Sorted::build(&data, weights).unwrap();
            for pattern in [">0", "<=0", "<>1", "<=1", ">1", "<2.6", ">=1"] {
                let crit = Crit::parse(pattern);
                let (op, n) = crit.numeric_bounds().unwrap();
                let expected = data
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| crit.matches_coerced(v))
                    .map(|(i, _)| weights.map_or(1., |w| w[i].to_number().unwrap()))
                    .sum();
                assert_eq!(sorted.query(op, n), FValue::Number(expected), "{pattern}");
            }
        }
        for unsafe_amounts in [
            vec![FValue::Number(0.1)],
            vec![FValue::Number(2f64.powi(53)), FValue::Number(1.)],
        ] {
            assert!(Sorted::build(&data, Some(&unsafe_amounts)).is_none());
        }
    }
}
