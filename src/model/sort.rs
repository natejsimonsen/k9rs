//! Column sorting (k9s shift-<col> keys), mirroring k9s `model1.Less`:
//! - metrics columns (`mx`): natural order after stripping `,`;
//! - time columns: durations (`1y2d3h4m5s`, `n/a` sorts last);
//! - capacity columns: resource quantities (`1Gi`, `500M`, `12e6`);
//! - everything else: natural order (`fvbommel/sortorder.NaturalLess`),
//!   which also orders IPs and versions sensibly.
//!
//! Equal cells fall back to the natural order of the row ids (`ns/name`).
//! k9s uses Go's unstable `sort.Sort` with a `<=` comparator; here the
//! comparator is a total order (primary key, then id), which yields the same
//! result whenever k9s' result is deterministic.

use std::cmp::Ordering;

use super::{Header, RowEvent, NA_VALUE};
use crate::config::ViewSetting;
use crate::k8s::is_all_namespaces;

/// Current sort state of a table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SortSpec {
    pub col: Option<usize>,
    pub asc: bool,
}

/// A sort column by name (model1.SortColumn).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SortColumn {
    pub name: String,
    pub asc: bool,
}

impl SortColumn {
    pub fn new(name: impl Into<String>, asc: bool) -> Self {
        Self {
            name: name.into(),
            asc,
        }
    }
    pub fn is_set(&self) -> bool {
        !self.name.is_empty()
    }
}

/// How a column's cells are compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKind {
    #[default]
    Text,
    Number,
    Duration,
    Capacity,
}

impl SortKind {
    /// The kind for column `col` of `h` (TableData.Sort: Time, MX, Capacity).
    pub fn for_col(h: &Header, col: usize) -> SortKind {
        match h.0.get(col) {
            Some(c) if c.mx => SortKind::Number,
            Some(c) if c.time => SortKind::Duration,
            Some(c) if c.capacity => SortKind::Capacity,
            _ => SortKind::Text,
        }
    }
}

/// Sort `rows` in place by column `col`.
pub fn sort_rows(rows: &mut [RowEvent], header: &Header, col: usize, asc: bool) {
    let kind = SortKind::for_col(header, col);
    sort_rows_by(rows, col, kind, asc);
}

/// Sort `rows` in place by column `col` using an explicit comparison kind.
pub fn sort_rows_by(rows: &mut [RowEvent], col: usize, kind: SortKind, asc: bool) {
    let empty = String::new();
    rows.sort_by(|a, b| {
        let v1 = a.row.fields.get(col).unwrap_or(&empty);
        let v2 = b.row.fields.get(col).unwrap_or(&empty);
        let o = compare(kind, &a.row.id, &b.row.id, v1, v2);
        if asc {
            o
        } else {
            o.reverse()
        }
    });
}

/// Sort by column name like model1.TableData.Sort: the column is looked up
/// by exact name among non-wide columns; unknown names are a no-op.
pub fn sort_by_column(rows: &mut [RowEvent], header: &Header, sc: &SortColumn) {
    if let Some(idx) = index_of(header, &sc.name, false) {
        sort_rows(rows, header, idx, sc.asc);
    }
}

/// Total-order comparison behind [`less`].
pub fn compare(kind: SortKind, id1: &str, id2: &str, v1: &str, v2: &str) -> Ordering {
    if v1 == v2 {
        return natural_cmp(id1, id2);
    }
    let o = match kind {
        SortKind::Number => natural_cmp(&v1.replace(',', ""), &v2.replace(',', "")),
        SortKind::Duration => duration_to_seconds(v1).cmp(&duration_to_seconds(v2)),
        SortKind::Capacity => capacity_to_number(v1).cmp(&capacity_to_number(v2)),
        SortKind::Text => natural_cmp(v1, v2),
    };
    o.then_with(|| natural_cmp(id1, id2))
}

/// model1.Less: true when `v1` sorts at or before `v2` (ids break ties).
pub fn less(kind: SortKind, id1: &str, id2: &str, v1: &str, v2: &str) -> bool {
    if v1 == v2 {
        return natural_less(id1, id2);
    }
    match kind {
        SortKind::Number => natural_less(&v1.replace(',', ""), &v2.replace(',', "")),
        SortKind::Duration => duration_to_seconds(v1) <= duration_to_seconds(v2),
        SortKind::Capacity => capacity_to_number(v1) <= capacity_to_number(v2),
        SortKind::Text => natural_less(v1, v2),
    }
}

/// sortorder.NaturalLess (case-sensitive, byte-wise for non digits).
pub fn natural_less(s1: &str, s2: &str) -> bool {
    natural_cmp(s1, s2) == Ordering::Less
}

/// Ordering form of sortorder.NaturalLess: digit runs compare numerically
/// (fewer leading zeros first on ties), digits sort before other bytes.
pub fn natural_cmp(s1: &str, s2: &str) -> Ordering {
    let (b1, b2) = (s1.as_bytes(), s2.as_bytes());
    let (mut i1, mut i2) = (0, 0);
    while i1 < b1.len() && i2 < b2.len() {
        let (c1, c2) = (b1[i1], b2[i2]);
        let (d1, d2) = (c1.is_ascii_digit(), c2.is_ascii_digit());
        if d1 != d2 {
            return if d1 {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        if !d1 {
            if c1 != c2 {
                return c1.cmp(&c2);
            }
            i1 += 1;
            i2 += 1;
            continue;
        }
        while i1 < b1.len() && b1[i1] == b'0' {
            i1 += 1;
        }
        while i2 < b2.len() && b2[i2] == b'0' {
            i2 += 1;
        }
        let (nz1, nz2) = (i1, i2);
        while i1 < b1.len() && b1[i1].is_ascii_digit() {
            i1 += 1;
        }
        while i2 < b2.len() && b2[i2].is_ascii_digit() {
            i2 += 1;
        }
        let (l1, l2) = (i1 - nz1, i2 - nz2);
        if l1 != l2 {
            return l1.cmp(&l2);
        }
        let (n1, n2) = (&b1[nz1..i1], &b2[nz2..i2]);
        if n1 != n2 {
            return n1.cmp(n2);
        }
        if nz1 != nz2 {
            return nz1.cmp(&nz2);
        }
    }
    b1.len().cmp(&b2.len())
}

/// model1.durationToSeconds: `1y2d3h4m5s` → seconds. `n/a` → i64::MAX,
/// "" → 0. Other characters are folded in digit-wise exactly like k9s.
pub fn duration_to_seconds(duration: &str) -> i64 {
    if duration.is_empty() {
        return 0;
    }
    if duration == NA_VALUE {
        return i64::MAX;
    }
    let mut num: Vec<char> = Vec::with_capacity(5);
    let mut n: i64 = 0;
    for r in duration.chars() {
        let m: i64 = match r {
            'y' => 365 * 24 * 60 * 60,
            'd' => 24 * 60 * 60,
            'h' => 60 * 60,
            'm' => 60,
            's' => 1,
            _ => {
                num.push(r);
                continue;
            }
        };
        n = n.wrapping_add(runes_to_num(&num).wrapping_mul(m));
        num.clear();
    }
    n
}

fn runes_to_num(rr: &[char]) -> i64 {
    let mut r: i64 = 0;
    let mut m: i64 = 1;
    for &c in rr.iter().rev() {
        let v = c as i64 - '0' as i64;
        r = r.wrapping_add(v.wrapping_mul(m));
        m = m.wrapping_mul(10);
    }
    r
}

/// Resource quantity value rounded up away from zero (resource.Quantity.Value).
/// Unparseable quantities yield `None`.
pub fn parse_quantity(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    let mut i = 0;
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let mut mantissa: i128 = 0;
    let mut frac_digits: i32 = 0;
    let mut digits = 0;
    let mut seen_dot = false;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        if b[i] == b'.' {
            if seen_dot {
                return None;
            }
            seen_dot = true;
        } else {
            digits += 1;
            if mantissa < i128::MAX / 100 {
                mantissa = mantissa * 10 + (b[i] - b'0') as i128;
                if seen_dot {
                    frac_digits += 1;
                }
            } else {
                // Precision beyond i128 is dropped; keep the magnitude.
                if !seen_dot {
                    frac_digits -= 1;
                }
            }
        }
        i += 1;
    }
    if digits == 0 {
        return None;
    }
    let suffix = &s[i..];
    let (exp10, exp2): (i32, u32) = match suffix {
        "" => (0, 0),
        "Ki" => (0, 10),
        "Mi" => (0, 20),
        "Gi" => (0, 30),
        "Ti" => (0, 40),
        "Pi" => (0, 50),
        "Ei" => (0, 60),
        "n" => (-9, 0),
        "u" => (-6, 0),
        "m" => (-3, 0),
        "k" => (3, 0),
        "M" => (6, 0),
        "G" => (9, 0),
        "T" => (12, 0),
        "P" => (15, 0),
        "E" => (18, 0),
        _ => {
            let rest = suffix.strip_prefix(['e', 'E'])?;
            (rest.parse().ok()?, 0)
        }
    };
    let exp = exp10 - frac_digits;
    let mut num: i128 = mantissa;
    let mut den: i128 = 1;
    let sat = if neg { i64::MIN } else { i64::MAX };
    for _ in 0..exp2 {
        num = match num.checked_mul(2) {
            Some(v) => v,
            None => return Some(sat),
        };
    }
    if exp >= 0 {
        for _ in 0..exp {
            num = match num.checked_mul(10) {
                Some(v) => v,
                None => return Some(sat),
            };
        }
    } else {
        for _ in 0..(-exp) {
            den = match den.checked_mul(10) {
                Some(v) => v,
                None => return Some(0),
            };
        }
    }
    let mut v = num / den;
    if num % den != 0 {
        v += 1;
    }
    if neg {
        v = -v;
    }
    Some(v.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

/// model1.capacityToNumber. k9s panics on unparseable quantities; here they
/// sort as 0.
pub fn capacity_to_number(capacity: &str) -> i64 {
    parse_quantity(capacity).unwrap_or(0)
}

/// Header.IndexOf(name, includeWide): exact name match.
fn index_of(h: &Header, name: &str, include_wide: bool) -> Option<usize> {
    h.iter()
        .position(|c| (include_wide || !c.wide) && c.name == name)
}

/// ViewSetting.SortCol: parse `NAME:asc|desc`.
pub fn view_sort_col(vs: &ViewSetting) -> Option<SortColumn> {
    if vs.sort_column.is_empty() {
        return None;
    }
    let tt: Vec<&str> = vs.sort_column.split(':').collect();
    if tt.len() < 2 {
        return None;
    }
    Some(SortColumn::new(tt[0], tt[1] == "asc"))
}

/// k9s default sort column (TableData.sortCol): the view setting's column
/// when present in the header, else NAMESPACE (all namespaces) or NAME, else
/// the first column; ascending. `None` for an empty header.
pub fn default_sort_col(h: &Header, ns: &str, vs: Option<&ViewSetting>) -> Option<SortColumn> {
    if h.is_empty() {
        return None;
    }
    if let Some(sc) = vs.and_then(view_sort_col) {
        if index_of(h, &sc.name, false).is_some() {
            return Some(sc);
        }
    }
    let name = if is_all_namespaces(ns) {
        if index_of(h, "NAMESPACE", false).is_some() {
            "NAMESPACE".to_string()
        } else if index_of(h, "NAME", false).is_some() {
            "NAME".to_string()
        } else {
            String::new()
        }
    } else if index_of(h, "NAME", false).is_some() {
        "NAME".to_string()
    } else {
        h.0[0].name.clone()
    };
    Some(SortColumn::new(name, true))
}

/// TableData.ComputeSortCol: pick the effective sort column given the view
/// setting `vs`, the current column `sc` and whether the user sorted manually.
pub fn compute_sort_col(
    h: &Header,
    ns: &str,
    vs: Option<&ViewSetting>,
    sc: &SortColumn,
    manual: bool,
) -> SortColumn {
    let blank = vs.is_none_or(|v| v.columns.is_empty() && v.sort_column.is_empty());
    if blank {
        if !sc.name.is_empty() {
            return sc.clone();
        }
        return default_sort_col(h, ns, vs).unwrap_or_else(|| sc.clone());
    }
    if manual && sc.is_set() {
        return sc.clone();
    }
    vs.and_then(view_sort_col).unwrap_or_else(|| sc.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HeaderColumn, ResEvent, Row};

    fn rows(rr: &[(&str, &[&str])]) -> Vec<RowEvent> {
        rr.iter()
            .map(|(id, ff)| {
                RowEvent::new(
                    ResEvent::Unchanged,
                    Row::with(*id, ff.iter().map(|s| s.to_string()).collect()),
                )
            })
            .collect()
    }
    fn ids(rr: &[RowEvent]) -> Vec<&str> {
        rr.iter().map(|r| r.row.id.as_str()).collect()
    }
    fn col0(rr: &[RowEvent]) -> Vec<&str> {
        rr.iter().map(|r| r.row.fields[0].as_str()).collect()
    }

    #[test]
    fn natural_less_cases() {
        // fvbommel/sortorder TestNaturalLess
        let cases = [
            ("0", "00", true),
            ("aa", "ab", true),
            ("ab", "abc", true),
            ("abc", "ad", true),
            ("ab1", "ab2", true),
            ("ab1c", "ab1c", false),
            ("ab12", "abc", true),
            ("ab2a", "ab10", true),
            ("a0001", "a0000001", true),
            ("a10", "abcdefgh2", true),
            ("аб2аб", "аб10аб", true),
            ("2аб", "3аб", true),
            ("a1b", "a01b", true),
            ("ab01b", "ab010b", true),
            ("a01b001", "a001b01", true),
            ("a1", "a1x", true),
            ("1ax", "1b", true),
            ("082", "83", true),
            ("9a", "083a", true),
            ("10.0.0.9", "10.0.0.10", true),
            ("v1.9.0", "v1.10.0", true),
        ];
        for (a, b, e) in cases {
            assert_eq!(natural_less(a, b), e, "{a} < {b}");
            if e {
                assert!(!natural_less(b, a), "{b} < {a}");
            }
        }
        let mut v = vec!["abc5", "abc1", "abc01", "ab", "abc10", "abc2"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["ab", "abc1", "abc01", "abc2", "abc5", "abc10"]);
    }

    #[test]
    fn duration_to_seconds_cases() {
        let cases = [
            ("22s", 22),
            ("22m", 1320),
            ("12h", 43200),
            ("3d", 259200),
            ("3d9h", 291600),
            ("2d22h3m", 252180),
            ("2d22h3m50s", 252230),
            ("3y", 94608000),
            ("1y2d", 31708800),
            ("n/a", i64::MAX),
            ("", 0),
        ];
        for (s, e) in cases {
            assert_eq!(duration_to_seconds(s), e, "{s}");
        }
    }

    #[test]
    fn quantity_cases() {
        let cases = [
            ("1234", Some(1234)),
            ("12e6", Some(12_000_000)),
            ("1Gi", Some(1 << 30)),
            ("1.1G", Some(1_100_000_000)),
            ("0.5Ti", Some(1 << 39)),
            ("0.1Ei", Some(115_292_150_460_684_698)),
            ("10G", Some(10_000_000_000)),
            ("100m", Some(1)),
            ("1500m", Some(2)),
            ("-1500m", Some(-2)),
            ("1k", Some(1000)),
            ("5Mi", Some(5 << 20)),
            ("", None),
            ("n/a", None),
            ("1Zi", None),
            ("1.2.3", None),
        ];
        for (s, e) in cases {
            assert_eq!(parse_quantity(s), e, "{s}");
        }
    }

    #[test]
    fn less_cases() {
        // model1 TestLess
        assert!(!less(SortKind::Duration, "id1", "id2", "2y263d", "1y179d"));
        assert!(!less(SortKind::Duration, "id1", "id2", "2y263d", "19h"));
        assert!(!less(SortKind::Capacity, "id1", "id2", "1Gi", "1G"));
        assert!(less(SortKind::Capacity, "id1", "id2", "1Gi", "1Ti"));
        assert!(less(SortKind::Text, "a", "b", "x", "x"));
        assert!(!less(SortKind::Text, "b", "a", "x", "x"));
    }

    #[test]
    fn sort_text() {
        let h = Header::new(vec![HeaderColumn::new("A"), HeaderColumn::new("B")]);
        let mut r = rows(&[("1", &["blee", "duh"]), ("2", &["albert", "blee"])]);
        sort_rows(&mut r, &h, 0, true);
        assert_eq!(col0(&r), vec!["albert", "blee"]);
        sort_rows(&mut r, &h, 0, false);
        assert_eq!(col0(&r), vec!["blee", "albert"]);
        let mut r = rows(&[("1", &["blee-duh", "duh"]), ("2", &["blee", "blee"])]);
        sort_rows(&mut r, &h, 0, true);
        assert_eq!(col0(&r), vec!["blee", "blee-duh"]);
        let mut r = rows(&[("1", &["10", "duh"]), ("2", &["1", "blee"])]);
        sort_rows(&mut r, &h, 0, true);
        assert_eq!(col0(&r), vec!["1", "10"]);
        sort_rows(&mut r, &h, 0, false);
        assert_eq!(col0(&r), vec!["10", "1"]);
    }

    #[test]
    fn sort_duration() {
        let h = Header::new(vec![
            HeaderColumn::new("AGE").time(),
            HeaderColumn::new("B"),
        ]);
        let mut r = rows(&[("1", &["2m24s", "blee"]), ("2", &["2m12s", "duh"])]);
        sort_rows(&mut r, &h, 0, true);
        assert_eq!(col0(&r), vec!["2m12s", "2m24s"]);
        let mut r = rows(&[
            ("1", &["n/a", "x"]),
            ("2", &["3d", "x"]),
            ("3", &["5m", "x"]),
        ]);
        sort_rows(&mut r, &h, 0, true);
        assert_eq!(col0(&r), vec!["5m", "3d", "n/a"]);
        sort_rows(&mut r, &h, 0, false);
        assert_eq!(col0(&r), vec!["n/a", "3d", "5m"]);
    }

    #[test]
    fn sort_metrics() {
        let h = Header::new(vec![
            HeaderColumn::new("CPU").mxc(),
            HeaderColumn::new("MEM").mxm(),
        ]);
        let mut r = rows(&[("1", &["10m", "duh"]), ("2", &["1m", "blee"])]);
        sort_rows(&mut r, &h, 0, true);
        assert_eq!(col0(&r), vec!["1m", "10m"]);
        let mut r = rows(&[("1", &["10000m", "1000Mi"]), ("2", &["1m", "50Mi"])]);
        sort_rows(&mut r, &h, 1, false);
        assert_eq!(col0(&r), vec!["10000m", "1m"]);
        let mut r = rows(&[
            ("1", &["1,200", "x"]),
            ("2", &["900", "x"]),
            ("3", &["n/a", "x"]),
        ]);
        sort_rows(&mut r, &h, 0, true);
        assert_eq!(col0(&r), vec!["900", "1,200", "n/a"]);
    }

    #[test]
    fn sort_capacity() {
        let h = Header::new(vec![
            HeaderColumn::new("NAME"),
            HeaderColumn::new("B"),
            HeaderColumn::new("C"),
            HeaderColumn::new("CAPACITY").capacity(),
        ]);
        let mut r = rows(&[
            ("ns1/B", &["B", "2", "3", "1Gi"]),
            ("ns1/A", &["A", "2", "3", "1.1G"]),
            ("ns1/C", &["C", "2", "3", "0.5Ti"]),
            ("ns2/B", &["B", "2", "3", "12e6"]),
            ("ns2/A", &["A", "2", "3", "1234"]),
            ("ns2/C", &["C", "2", "3", "0.1Ei"]),
        ]);
        sort_rows(&mut r, &h, 3, true);
        assert_eq!(
            ids(&r),
            vec!["ns2/A", "ns2/B", "ns1/B", "ns1/A", "ns1/C", "ns2/C"]
        );
        let mut r = rows(&[("1", &["x", "", "", "10Gi"]), ("2", &["y", "", "", "10G"])]);
        sort_rows(&mut r, &h, 3, true);
        assert_eq!(ids(&r), vec!["2", "1"]);
    }

    #[test]
    fn sort_id_tiebreak() {
        // model1 TestRowEventsSort id_preserve
        let h = Header::new(vec![
            HeaderColumn::new("A"),
            HeaderColumn::new("B"),
            HeaderColumn::new("C"),
        ]);
        let mut r = rows(&[
            ("ns1/B", &["B", "2", "3"]),
            ("ns1/A", &["A", "2", "3"]),
            ("ns1/C", &["C", "2", "3"]),
            ("ns2/B", &["B", "2", "3"]),
            ("ns2/A", &["A", "2", "3"]),
            ("ns2/C", &["C", "2", "3"]),
        ]);
        sort_rows(&mut r, &h, 1, true);
        assert_eq!(
            ids(&r),
            vec!["ns1/A", "ns1/B", "ns1/C", "ns2/A", "ns2/B", "ns2/C"]
        );
        let mut r = rows(&[
            ("A", &["1", "2", "3"]),
            ("B", &["0", "2", "3"]),
            ("C", &["10", "2", "3"]),
        ]);
        sort_rows(&mut r, &h, 0, true);
        assert_eq!(ids(&r), vec!["B", "A", "C"]);
        // Out of range columns compare as blanks.
        sort_rows(&mut r, &h, 9, true);
        assert_eq!(ids(&r), vec!["A", "B", "C"]);
    }

    #[test]
    fn sort_by_column_name_skips_wide() {
        let h = Header::new(vec![
            HeaderColumn::new("NAME"),
            HeaderColumn::new("IP").wide(),
        ]);
        let mut r = rows(&[("b", &["b", "1"]), ("a", &["a", "2"])]);
        sort_by_column(&mut r, &h, &SortColumn::new("IP", false));
        assert_eq!(ids(&r), vec!["b", "a"]);
        sort_by_column(&mut r, &h, &SortColumn::new("NAME", true));
        assert_eq!(ids(&r), vec!["a", "b"]);
    }

    fn vs(cols: &[&str], sort: &str) -> ViewSetting {
        ViewSetting {
            columns: cols.iter().map(|s| s.to_string()).collect(),
            sort_column: sort.into(),
        }
    }

    #[test]
    fn compute_sort_col_cases() {
        // model1 TestTableDataComputeSortCol
        let h = Header::new(vec![
            HeaderColumn::new("A"),
            HeaderColumn::new("B"),
            HeaderColumn::new("C"),
        ]);
        let hw = Header::new(vec![
            HeaderColumn::new("A"),
            HeaderColumn::new("B").wide(),
            HeaderColumn::new("C"),
        ]);
        let none = SortColumn::default();
        assert_eq!(
            compute_sort_col(&h, "", Some(&vs(&["A", "B", "C"], "A:asc")), &none, false),
            SortColumn::new("A", true)
        );
        assert_eq!(
            compute_sort_col(&hw, "", Some(&vs(&["A", "B", "C"], "B:desc")), &none, false),
            SortColumn::new("B", false)
        );
        assert_eq!(
            compute_sort_col(&hw, "", Some(&vs(&["A", "C"], "")), &none, false),
            SortColumn::new("", false)
        );
        // Manual sort wins over view settings.
        let man = SortColumn::new("C", false);
        assert_eq!(
            compute_sort_col(&h, "", Some(&vs(&["A"], "A:asc")), &man, true),
            man
        );
        // Blank view settings: keep the current column, else defaults.
        assert_eq!(compute_sort_col(&h, "", None, &man, false), man);
        assert_eq!(
            compute_sort_col(&h, "", None, &none, false),
            SortColumn::new("", true)
        );
    }

    #[test]
    fn default_sort_col_cases() {
        let pods = Header::new(vec![
            HeaderColumn::new("NAMESPACE"),
            HeaderColumn::new("NAME"),
            HeaderColumn::new("AGE").time(),
        ]);
        assert_eq!(
            default_sort_col(&pods, "", None),
            Some(SortColumn::new("NAMESPACE", true))
        );
        assert_eq!(
            default_sort_col(&pods, "all", None),
            Some(SortColumn::new("NAMESPACE", true))
        );
        assert_eq!(
            default_sort_col(&pods, "default", None),
            Some(SortColumn::new("NAME", true))
        );
        assert_eq!(
            default_sort_col(&pods, "default", Some(&vs(&[], "AGE:desc"))),
            Some(SortColumn::new("AGE", false))
        );
        let other = Header::new(vec![HeaderColumn::new("KEY"), HeaderColumn::new("VALUE")]);
        assert_eq!(
            default_sort_col(&other, "default", None),
            Some(SortColumn::new("KEY", true))
        );
        assert_eq!(
            default_sort_col(&other, "", None),
            Some(SortColumn::new("", true))
        );
        assert_eq!(default_sort_col(&Header::default(), "", None), None);
    }
}
