//! Port of `github.com/sahilm/fuzzy` (v0.1.1) `Find`, used by k9s for `-f`
//! filters, plus Go's `sort.Stable` so ties are ordered exactly like k9s
//! (sahilm's `Less` is `>=`, which makes tie order algorithm dependent).

const FIRST_CHAR_MATCH_BONUS: i64 = 10;
const MATCH_FOLLOWING_SEPARATOR_BONUS: i64 = 20;
const CAMEL_CASE_MATCH_BONUS: i64 = 20;
const ADJACENT_MATCH_BONUS: i64 = 5;
const UNMATCHED_LEADING_CHAR_PENALTY: i64 = -5;
const MAX_UNMATCHED_LEADING_CHAR_PENALTY: i64 = -15;

/// A fuzzy match: index into the searched data, score and matched byte offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub index: usize,
    pub score: i64,
    pub matched: Vec<usize>,
}

fn is_separator(c: char) -> bool {
    matches!(c, '/' | '-' | '_' | ' ' | '.' | '\\')
}

fn equal_fold(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(&b);
    }
    let mut la = a.to_lowercase();
    let mut lb = b.to_lowercase();
    let single = |it: &mut std::char::ToLowercase| -> Option<char> {
        let c = it.next()?;
        if it.next().is_some() {
            None
        } else {
            Some(c)
        }
    };
    match (single(&mut la), single(&mut lb)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// fuzzy.Find: matches of `pattern` in `data`, best score first.
pub fn find(pattern: &str, data: &[&str]) -> Vec<Match> {
    if pattern.is_empty() {
        return Vec::new();
    }
    let runes: Vec<char> = pattern.chars().collect();
    let mut matches = Vec::new();
    for (i, s) in data.iter().enumerate() {
        let mut m = Match {
            index: i,
            score: 0,
            matched: Vec::with_capacity(runes.len()),
        };
        let mut pattern_index = 0usize;
        let mut best_score: i64 = -1;
        let mut matched_index: i64 = -1;
        let mut curr_adjacent_bonus: i64 = 0;
        let mut last: char = '\0';
        let mut last_index: usize = 0;
        let chars: Vec<(usize, char)> = s.char_indices().collect();
        for (k, &(j, candidate)) in chars.iter().enumerate() {
            if pattern_index >= runes.len() {
                break;
            }
            if equal_fold(candidate, runes[pattern_index]) {
                let mut score = 0;
                if j == 0 {
                    score += FIRST_CHAR_MATCH_BONUS;
                }
                if last.is_lowercase() && candidate.is_uppercase() {
                    score += CAMEL_CASE_MATCH_BONUS;
                }
                if j != 0 && is_separator(last) {
                    score += MATCH_FOLLOWING_SEPARATOR_BONUS;
                }
                if let Some(&last_match) = m.matched.last() {
                    let bonus = if last_match == last_index {
                        curr_adjacent_bonus * 2 + ADJACENT_MATCH_BONUS
                    } else {
                        0
                    };
                    score += bonus;
                    curr_adjacent_bonus += bonus;
                }
                if score > best_score {
                    best_score = score;
                    matched_index = j as i64;
                }
            }
            let nextp = runes.get(pattern_index + 1).copied().unwrap_or('\0');
            let nextc = chars.get(k + 1).map(|&(_, c)| c).unwrap_or('\0');
            if (equal_fold(nextp, nextc) || nextc == '\0') && matched_index > -1 {
                if m.matched.is_empty() {
                    let penalty = matched_index * UNMATCHED_LEADING_CHAR_PENALTY;
                    best_score += penalty.max(MAX_UNMATCHED_LEADING_CHAR_PENALTY);
                }
                m.score += best_score;
                m.matched.push(matched_index as usize);
                best_score = -1;
                pattern_index += 1;
            }
            last_index = j;
            last = candidate;
        }
        m.score += m.matched.len() as i64 - s.len() as i64;
        if m.matched.len() == runes.len() {
            matches.push(m);
        }
    }
    go_stable_sort(&mut matches, |a, b| a.score >= b.score);
    matches
}

/// Go's `sort.Stable` (insertion sort blocks of 20 + SymMerge), driven by a
/// Go-style `less`. Reproduces Go's element order even for non-strict `less`.
pub fn go_stable_sort<T>(v: &mut [T], less: impl Fn(&T, &T) -> bool) {
    let n = v.len();
    let mut block = 20;
    let (mut a, mut b) = (0, block);
    while b <= n {
        insertion_sort(v, a, b, &less);
        a = b;
        b += block;
    }
    insertion_sort(v, a, n, &less);
    while block < n {
        a = 0;
        b = 2 * block;
        while b <= n {
            sym_merge(v, a, a + block, b, &less);
            a = b;
            b += 2 * block;
        }
        let m = a + block;
        if m < n {
            sym_merge(v, a, m, n, &less);
        }
        block *= 2;
    }
}

fn insertion_sort<T>(v: &mut [T], a: usize, b: usize, less: &impl Fn(&T, &T) -> bool) {
    for i in a + 1..b {
        let mut j = i;
        while j > a && less(&v[j], &v[j - 1]) {
            v.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn sym_merge<T>(v: &mut [T], a: usize, m: usize, b: usize, less: &impl Fn(&T, &T) -> bool) {
    if m - a == 1 {
        let (mut i, mut j) = (m, b);
        while i < j {
            let h = (i + j) / 2;
            if less(&v[h], &v[a]) {
                i = h + 1;
            } else {
                j = h;
            }
        }
        let mut k = a;
        while k + 1 < i {
            v.swap(k, k + 1);
            k += 1;
        }
        return;
    }
    if b - m == 1 {
        let (mut i, mut j) = (a, m);
        while i < j {
            let h = (i + j) / 2;
            if !less(&v[m], &v[h]) {
                i = h + 1;
            } else {
                j = h;
            }
        }
        let mut k = m;
        while k > i {
            v.swap(k, k - 1);
            k -= 1;
        }
        return;
    }
    let mid = (a + b) / 2;
    let n = mid + m;
    let (mut start, mut r) = if m > mid { (n - b, mid) } else { (a, m) };
    let p = n - 1;
    while start < r {
        let c = (start + r) / 2;
        if !less(&v[p - c], &v[c]) {
            start = c + 1;
        } else {
            r = c;
        }
    }
    let end = n - start;
    if start < m && m < end {
        rotate(v, start, m, end);
    }
    if a < start && start < mid {
        sym_merge(v, a, start, mid, less);
    }
    if mid < end && end < b {
        sym_merge(v, mid, end, b, less);
    }
}

fn swap_range<T>(v: &mut [T], a: usize, b: usize, n: usize) {
    for i in 0..n {
        v.swap(a + i, b + i);
    }
}

fn rotate<T>(v: &mut [T], a: usize, m: usize, b: usize) {
    let mut i = m - a;
    let mut j = b - m;
    while i != j {
        if i > j {
            swap_range(v, m - i, m, j);
            i -= j;
        } else {
            swap_range(v, m - i, m + j - i, i);
            j -= i;
        }
    }
    swap_range(v, m - i, m, i);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(mm: &[Match], data: &[&str]) -> Vec<String> {
        mm.iter().map(|m| data[m.index].to_string()).collect()
    }

    #[test]
    fn find_basic() {
        let data = ["game.cpp", "moduleNameResolver.ts", "my name is_Ramsey"];
        let mm = find("mnr", &data);
        // Scores: "my name is_Ramsey" 36, "moduleNameResolver.ts" 32.
        assert_eq!(
            strs(&mm, &data),
            vec!["my name is_Ramsey", "moduleNameResolver.ts"]
        );
        assert_eq!((mm[0].score, mm[1].score), (36, 32));
        assert_eq!(mm[0].matched, vec![0, 3, 11]);
        assert_eq!(mm[1].matched, vec![0, 6, 10]);
    }

    #[test]
    fn find_exhaustive_best_match() {
        let data = ["The Black Knight"];
        let mm = find("tk", &data);
        assert_eq!(mm.len(), 1);
        assert_eq!(mm[0].matched, vec![0, 10]);
    }

    #[test]
    fn find_case_insensitive_and_unicode() {
        let data = ["FooBar", "foobar", "zzz", "ÄBC"];
        assert_eq!(strs(&find("fb", &data), &data).len(), 2);
        assert_eq!(strs(&find("äb", &data), &data), vec!["ÄBC"]);
        assert!(find("", &data).is_empty());
        assert!(find("q", &data).is_empty());
    }

    #[test]
    fn find_ranks_prefix_first() {
        let data = ["default/zorg-fred", "default/fred", "kube/f-r-e-d"];
        let mm = find("fred", &data);
        assert_eq!(strs(&mm, &data)[0], "default/fred");
        assert_eq!(mm.len(), 3);
    }

    #[test]
    fn stable_sort_matches_go() {
        // Strict less behaves like a stable sort.
        let mut v: Vec<(i32, usize)> = (0..100).map(|i| ((i * 37 % 7) as i32, i)).collect();
        let mut e = v.clone();
        e.sort_by_key(|x| x.0);
        go_stable_sort(&mut v, |a, b| a.0 < b.0);
        assert_eq!(v, e);
        // Go's insertion sort with a `>=` less reverses equal runs within a block.
        let mut w = vec![(1, 'a'), (1, 'b'), (1, 'c')];
        go_stable_sort(&mut w, |a, b| a.0 >= b.0);
        assert_eq!(w, vec![(1, 'c'), (1, 'b'), (1, 'a')]);
    }
}
