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

/// Predicates on the same input column share row positions across aggregates.
/// Intersections preserve source order, including for non-associative floating sums.
#[derive(Default)]
pub(super) struct MatchIndexes {
    columns: VecDeque<ColumnIndex>,
    masks: VecDeque<(RangeKey, String, std::rc::Rc<[u64]>)>,
}

struct ColumnIndex {
    range: RangeKey,
    len: usize,
    numbers: Option<Vec<(f64,u32)>>,
    text: Option<std::collections::HashMap<String,Vec<u32>>>,
    tried_numbers: bool,
    tried_text: bool,
}

impl ColumnIndex {
    fn mark(&mut self, input: &[FValue], criterion: &Crit, bits: &mut [u64]) {
        let mut set = |row: u32| bits[row as usize / 64] |= 1 << (row % 64);
        if let Some((op,n)) = criterion.numeric_query() {
            if !self.tried_numbers {
                self.tried_numbers = true;
                let mut sorted = Vec::new();
                let mut finite = true;
                for (i,value) in input.iter().enumerate() {
                    if let Ok(v) = value.to_number() {
                        if !v.is_finite() { finite = false; break; }
                        sorted.push((v,i as u32));
                    }
                }
                if finite {
                    sorted.sort_unstable_by(|a,b| a.0.total_cmp(&b.0));
                    self.numbers = Some(sorted);
                }
            }
            if n.is_finite() && let Some(sorted) = &self.numbers {
                let eq = || sorted.partition_point(|x| x.0-n < -f64::EPSILON)
                    ..sorted.partition_point(|x| x.0-n <= f64::EPSILON);
                let hits = match op {
                    ">" => sorted.partition_point(|x| x.0 <= n)..sorted.len(),
                    ">=" => sorted.partition_point(|x| x.0 < n)..sorted.len(),
                    "<" => 0..sorted.partition_point(|x| x.0 < n),
                    "<=" => 0..sorted.partition_point(|x| x.0 <= n),
                    _ => eq(),
                };
                for &(_,row) in &sorted[hits] { set(row); }
                if op == "<>" { for word in bits { *word = !*word; } }
                return;
            }
        } else {
            if !self.tried_text {
                self.tried_text = true;
                let mut dictionary = std::collections::HashMap::<String,Vec<u32>>::new();
                let mut bytes = 0usize;
                for (i,v) in input.iter().enumerate() {
                    let text = match v {
                        FValue::Text(t) => t.clone(), FValue::Bool(b) => b.to_string(),
                        FValue::Empty => String::new(), _ => v.display(),
                    };
                    if !dictionary.contains_key(&text) { bytes += text.len(); }
                    dictionary.entry(text).or_default().push(i as u32);
                    if dictionary.len() > 65_536 || bytes > 8*1024*1024 { break; }
                }
                if dictionary.len() <= 65_536 && bytes <= 8*1024*1024 { self.text = Some(dictionary); }
            }
            if let Some(words) = &self.text {
                for (word,rows) in words {
                    if criterion.matches_coerced_text(word) {
                        for &row in rows { set(row); }
                    }
                }
                return;
            }
        }
        for (i,v) in input.iter().enumerate() {
            if criterion.matches_coerced(v) { set(i as u32); }
        }
    }
}

impl MatchIndexes {
    fn mask(&mut self, range: RangeKey, pattern: &str, input: &[FValue]) -> std::rc::Rc<[u64]> {
        if let Some((_,_,bits)) = self.masks.iter().find(|(r,p,_)| *r == range && p == pattern) {
            return bits.clone();
        }
        let mut column = self.columns.iter().position(|c| c.range == range)
            .map(|i| self.columns.remove(i).unwrap()).unwrap_or(ColumnIndex {
                range, len: input.len(), numbers: None, text: None, tried_numbers: false, tried_text: false,
            });
        let mut bits = vec![0;input.len().div_ceil(64)];
        column.mark(input,&Crit::parse(pattern),&mut bits);
        while self.columns.len() >= 16 || self.columns.iter().map(|c| c.len).sum::<usize>() + input.len() > 4_194_304 {
            self.columns.pop_front();
        }
        self.columns.push_back(column);
        let bits: std::rc::Rc<[u64]> = bits.into();
        if self.masks.len() >= 64 { self.masks.pop_front(); }
        self.masks.push_back((range,pattern.to_owned(),bits.clone()));
        bits
    }

    pub(super) fn lookup(&mut self, key: &CriteriaKey, args: &[Arg]) -> Option<FValue> {
        let count = key.function == 1;
        let average_if = key.function == 2;
        let pairs = if average_if { args.get(..2)? } else { args.get(usize::from(!count)..)? };
        if pairs.is_empty() || pairs.len() % 2 != 0 || pairs.len()/2 != key.criteria.len() { return None; }
        let columns = if average_if { key.ranges.get(..1)? } else { key.ranges.get(usize::from(!count)..)? };
        if columns.len() != key.criteria.len() { return None; }
        let values = if average_if { args.get(2).unwrap_or(&args[0]) } else { &args[0] }.slice();
        let len = pairs.chunks_exact(2).map(|p| p[0].slice().len()).min()?.min(values.len());
        if len < 256 || pairs.chunks_exact(2).any(|p| p[0].slice().len() > 2_097_152) { return None; }
        let masks: Vec<_> = columns.iter().zip(&key.criteria).zip(pairs.chunks_exact(2))
            .map(|((&r,p),args)| self.mask(r,p,args[0].slice())).collect();
        let mut hits = 0usize;
        let mut total = 0.;
        let mut best: Option<f64> = None;
        for i in 0..len.div_ceil(64) {
            let mut word = masks.iter().fold(u64::MAX,|v,m| v & m[i]);
            if i == len/64 { word &= (1u64 << (len%64))-1; }
            if count { hits += word.count_ones() as usize; continue; }
            while word != 0 {
                let row = i*64 + word.trailing_zeros() as usize;
                word &= word-1;
                if let Ok(n) = values[row].to_number() {
                    hits += 1;
                    match key.function {
                        4 => best = Some(best.map_or(n,|v| v.min(n))),
                        5 => best = Some(best.map_or(n,|v| v.max(n))),
                        _ => total += n,
                    }
                }
            }
        }
        Some(match key.function {
            1 => FValue::number(hits as f64),
            2 | 3 if hits == 0 => FValue::Error(super::ErrKind::Div0),
            2 | 3 => FValue::number(total / hits as f64),
            4 | 5 => FValue::number(best.unwrap_or(0.)),
            _ => FValue::number(total),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_predicates_match_serial_aggregates_across_columns_and_partial_words() {
        fn range(v: Vec<FValue>) -> Arg {
            Arg::Range {rows:v.len(),cols:1,values:std::rc::Rc::new(v)}
        }
        let n = 329;
        let amounts = range((0..n).map(|i| FValue::Number([1e16,0.125,-1e16,-0.25][i%4])).collect());
        let dates = range((0..n).map(|i| FValue::Number((i%17) as f64)).collect());
        let names = range((0..n).map(|i| FValue::Text(["Apple","ape","other",""][i%4].into())).collect());
        let mut cache = MatchIndexes::default();
        let col = |c| (0,0,c,n as i64-1,c);
        for (id,func) in ["SUMIFS","COUNTIFS","AVERAGEIF","AVERAGEIFS","MINIFS","MAXIFS"].into_iter().enumerate() {
            for condition in [">=3","<>5","<12"] {
                let p = Arg::Val(FValue::Text(condition.into()));
                let w = Arg::Val(FValue::Text("a*".into()));
                let (args, ranges, criteria) = match id {
                    1 => (vec![dates.clone(),p,names.clone(),w],vec![col(1),col(2)],vec![condition.into(),"a*".into()]),
                    2 => (vec![dates.clone(),p,amounts.clone()],vec![col(1),col(0)],vec![condition.into()]),
                    _ => (vec![amounts.clone(),dates.clone(),p,names.clone(),w],vec![col(0),col(1),col(2)],vec![condition.into(),"a*".into()]),
                };
                let key = CriteriaKey { function:id as u8,ranges,criteria };
                assert_eq!(cache.lookup(&key,&args), Some(super::super::functions::call(func,&args)), "{func} {condition}");
            }
        }
        assert_eq!(cache.columns.len(),2);
        assert_eq!(cache.masks.len(),4);
    }

    #[test]
    fn predicate_masks_match_mixed_types_epsilon_wildcards_and_eviction() {
        let samples = [FValue::Empty, FValue::Bool(false), FValue::Bool(true),
            FValue::number(1.),FValue::number(1.+f64::EPSILON),FValue::number(1.+2.*f64::EPSILON),
            FValue::number(-0.),FValue::Text(" 1 ".into()), FValue::Error(super::super::ErrKind::Div0),
            FValue::Text("ΟΣ".into()),FValue::Text("a*".into())];
        let col = |c, len| (0,0,c,len as i64-1,c);
        let mut cache=MatchIndexes::default();
        for (run, len) in [256,257,320,321,384,385].into_iter().enumerate() {
            let mut values:Vec<_>=(0..len).map(|i| samples[i%samples.len()].clone()).collect();
            if run%2 == 1 { values[len-1]=FValue::Number(f64::INFINITY); }
            let data=Arg::Range {rows:len, cols:1, values:std::rc::Rc::new(values)};
            let amount=Arg::Range {rows:len,cols:1,values:std::rc::Rc::new((0..len).map(|i|
                FValue::number([1e16,0.0625,-1e16,-0.125][i%4])).collect())};
            for p in ["=1","<>1",">=0","<1","<=1",">1","*ς","a~*","<>true","=false","<empty","<>#DIV/0!",""] {
                let q=Arg::Val(FValue::Text(p.to_owned()));
                let q2=Arg::Val(FValue::Text("<>never".into()));
                for (function,name) in [(0,"SUMIFS"),(1,"COUNTIFS"),(3,"AVERAGEIFS"),(4,"MINIFS"),(5,"MAXIFS")] {
                    let mut args=vec![data.clone(),q.clone(),data.clone(),q2.clone()];
                    let mut ranges=vec![col(0,len),col(0,len)];
                    if function != 1 { args.insert(0,amount.clone()); ranges.insert(0,col(1,len)); }
                    let key=CriteriaKey{function,ranges,criteria:vec![p.to_owned(),"<>never".into()]};
                    assert_eq!(cache.lookup(&key,&args),Some(super::super::functions::call(name,&args)),"{name} {p}, {len} rows");
                }
            }
        }
    }

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
