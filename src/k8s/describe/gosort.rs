//! Port of Go's `sort.Sort` / `sort.Slice` (pattern-defeating quicksort).
//! kubectl sorts with these unstable sorts, so elements that compare equal
//! end up in Go's exact order only if we use the same algorithm.

/// Sort `data` the way Go's `sort.Slice(data, less)` would.
pub(crate) fn sort_by<T>(data: &mut [T], less: impl Fn(&T, &T) -> bool) {
    let n = data.len();
    if n <= 1 {
        return;
    }
    let limit = bits_len(n);
    let mut s = Sorter { data, less: &less };
    s.pdqsort(0, n, limit);
}

fn bits_len(n: usize) -> usize {
    (usize::BITS - n.leading_zeros()) as usize
}

#[derive(PartialEq, Clone, Copy)]
enum Hint {
    Unknown,
    Increasing,
    Decreasing,
}

struct Sorter<'a, T, F: Fn(&T, &T) -> bool> {
    data: &'a mut [T],
    less: &'a F,
}

impl<T, F: Fn(&T, &T) -> bool> Sorter<'_, T, F> {
    fn less(&self, i: usize, j: usize) -> bool {
        (self.less)(&self.data[i], &self.data[j])
    }

    fn swap(&mut self, i: usize, j: usize) {
        self.data.swap(i, j);
    }

    fn insertion_sort(&mut self, a: usize, b: usize) {
        for i in a + 1..b {
            let mut j = i;
            while j > a && self.less(j, j - 1) {
                self.swap(j, j - 1);
                j -= 1;
            }
        }
    }

    fn sift_down(&mut self, lo: usize, hi: usize, first: usize) {
        let mut root = lo;
        loop {
            let mut child = 2 * root + 1;
            if child >= hi {
                break;
            }
            if child + 1 < hi && self.less(first + child, first + child + 1) {
                child += 1;
            }
            if !self.less(first + root, first + child) {
                return;
            }
            self.swap(first + root, first + child);
            root = child;
        }
    }

    fn heap_sort(&mut self, a: usize, b: usize) {
        let first = a;
        let hi = b - a;
        let mut i = (hi as isize - 1) / 2;
        while i >= 0 {
            self.sift_down(i as usize, hi, first);
            i -= 1;
        }
        let mut i = hi as isize - 1;
        while i >= 0 {
            self.swap(first, first + i as usize);
            self.sift_down(0, i as usize, first);
            i -= 1;
        }
    }

    fn pdqsort(&mut self, mut a: usize, mut b: usize, mut limit: usize) {
        const MAX_INSERTION: usize = 12;
        let mut was_balanced = true;
        let mut was_partitioned = true;
        loop {
            let length = b - a;
            if length <= MAX_INSERTION {
                self.insertion_sort(a, b);
                return;
            }
            if limit == 0 {
                self.heap_sort(a, b);
                return;
            }
            if !was_balanced {
                self.break_patterns(a, b);
                limit -= 1;
            }
            let (mut pivot, mut hint) = self.choose_pivot(a, b);
            if hint == Hint::Decreasing {
                self.reverse_range(a, b);
                pivot = (b - 1) - (pivot - a);
                hint = Hint::Increasing;
            }
            if was_balanced
                && was_partitioned
                && hint == Hint::Increasing
                && self.partial_insertion_sort(a, b)
            {
                return;
            }
            if a > 0 && !self.less(a - 1, pivot) {
                let mid = self.partition_equal(a, b, pivot);
                a = mid;
                continue;
            }
            let (mid, already) = self.partition(a, b, pivot);
            was_partitioned = already;
            let (left_len, right_len) = (mid - a, b - mid);
            let balance_threshold = length / 8;
            if left_len < right_len {
                was_balanced = left_len >= balance_threshold;
                self.pdqsort(a, mid, limit);
                a = mid + 1;
            } else {
                was_balanced = right_len >= balance_threshold;
                self.pdqsort(mid + 1, b, limit);
                b = mid;
            }
        }
    }

    fn partition(&mut self, a: usize, b: usize, pivot: usize) -> (usize, bool) {
        self.swap(a, pivot);
        let (mut i, mut j) = (a as isize + 1, b as isize - 1);
        while i <= j && self.less(i as usize, a) {
            i += 1;
        }
        while i <= j && !self.less(j as usize, a) {
            j -= 1;
        }
        if i > j {
            self.swap(j as usize, a);
            return (j as usize, true);
        }
        self.swap(i as usize, j as usize);
        i += 1;
        j -= 1;
        loop {
            while i <= j && self.less(i as usize, a) {
                i += 1;
            }
            while i <= j && !self.less(j as usize, a) {
                j -= 1;
            }
            if i > j {
                break;
            }
            self.swap(i as usize, j as usize);
            i += 1;
            j -= 1;
        }
        self.swap(j as usize, a);
        (j as usize, false)
    }

    fn partition_equal(&mut self, a: usize, b: usize, pivot: usize) -> usize {
        self.swap(a, pivot);
        let (mut i, mut j) = (a as isize + 1, b as isize - 1);
        loop {
            while i <= j && !self.less(a, i as usize) {
                i += 1;
            }
            while i <= j && self.less(a, j as usize) {
                j -= 1;
            }
            if i > j {
                break;
            }
            self.swap(i as usize, j as usize);
            i += 1;
            j -= 1;
        }
        i as usize
    }

    fn partial_insertion_sort(&mut self, a: usize, b: usize) -> bool {
        const MAX_STEPS: usize = 5;
        const SHORTEST_SHIFTING: usize = 50;
        let mut i = a + 1;
        for _ in 0..MAX_STEPS {
            while i < b && !self.less(i, i - 1) {
                i += 1;
            }
            if i == b {
                return true;
            }
            if b - a < SHORTEST_SHIFTING {
                return false;
            }
            self.swap(i, i - 1);
            if i - a >= 2 {
                let mut j = i - 1;
                while j >= 1 {
                    if !self.less(j, j - 1) {
                        break;
                    }
                    self.swap(j, j - 1);
                    j -= 1;
                }
            }
            if b - i >= 2 {
                let mut j = i + 1;
                while j < b {
                    if !self.less(j, j - 1) {
                        break;
                    }
                    self.swap(j, j - 1);
                    j += 1;
                }
            }
        }
        false
    }

    fn break_patterns(&mut self, a: usize, b: usize) {
        let length = b - a;
        if length >= 8 {
            let mut random = length as u64;
            let modulus = 1usize << bits_len(length);
            let start = a + (length / 4) * 2 - 1;
            for idx in start..=start + 2 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let mut other = (random as usize) & (modulus - 1);
                if other >= length {
                    other -= length;
                }
                self.swap(idx, a + other);
            }
        }
    }

    fn choose_pivot(&mut self, a: usize, b: usize) -> (usize, Hint) {
        const SHORTEST_NINTHER: usize = 50;
        const MAX_SWAPS: usize = 4 * 3;
        let l = b - a;
        let mut swaps = 0;
        let mut i = a + l / 4;
        let mut j = a + l / 4 * 2;
        let mut k = a + l / 4 * 3;
        if l >= 8 {
            if l >= SHORTEST_NINTHER {
                i = self.median_adjacent(i, &mut swaps);
                j = self.median_adjacent(j, &mut swaps);
                k = self.median_adjacent(k, &mut swaps);
            }
            j = self.median(i, j, k, &mut swaps);
        }
        match swaps {
            0 => (j, Hint::Increasing),
            MAX_SWAPS => (j, Hint::Decreasing),
            _ => (j, Hint::Unknown),
        }
    }

    fn order2(&self, a: usize, b: usize, swaps: &mut usize) -> (usize, usize) {
        if self.less(b, a) {
            *swaps += 1;
            return (b, a);
        }
        (a, b)
    }

    fn median(&self, a: usize, b: usize, c: usize, swaps: &mut usize) -> usize {
        let (a, b) = self.order2(a, b, swaps);
        let (b, c) = self.order2(b, c, swaps);
        let _ = c;
        let (_a, b) = self.order2(a, b, swaps);
        b
    }

    fn median_adjacent(&self, a: usize, swaps: &mut usize) -> usize {
        self.median(a - 1, a, a + 1, swaps)
    }

    fn reverse_range(&mut self, a: usize, b: usize) {
        let (mut i, mut j) = (a, b - 1);
        while i < j {
            self.swap(i, j);
            i += 1;
            j -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_like_a_sort() {
        let mut v: Vec<i32> = (0..500).map(|i| (i * 7919) % 503).collect();
        sort_by(&mut v, |a, b| a < b);
        let mut w = v.clone();
        w.sort();
        assert_eq!(v, w);
    }

    #[test]
    fn small_slices_are_stable() {
        let mut v = vec![(1, 'a'), (0, 'b'), (1, 'c'), (0, 'd')];
        sort_by(&mut v, |a, b| a.0 < b.0);
        assert_eq!(v, vec![(0, 'b'), (0, 'd'), (1, 'a'), (1, 'c')]);
    }

    #[test]
    fn large_slice_with_duplicates_is_sorted() {
        let mut v: Vec<(i32, i32)> = (0..20).map(|i| (i % 3, i)).collect();
        sort_by(&mut v, |a, b| a.0 < b.0);
        let keys: Vec<i32> = v.iter().map(|x| x.0).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }
}
