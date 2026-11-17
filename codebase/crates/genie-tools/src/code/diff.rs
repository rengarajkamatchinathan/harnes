//! Line diff (Myers) and unified-diff rendering for edit previews and tool results.

/// One line-level operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Same(usize, usize),
    Del(usize),
    Ins(usize),
}

/// Above this many lines on both sides, fall back to "delete all, insert all".
const MAX_MYERS: usize = 20_000;

/// Myers' O(ND) shortest edit script between `a` and `b`.
pub fn diff_lines(a: &[&str], b: &[&str]) -> Vec<Op> {
    // Trim the common prefix and suffix first (the common case for edits).
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut ops: Vec<Op> = (0..pre).map(|i| Op::Same(i, i)).collect();
    if am.len() + bm.len() > MAX_MYERS {
        ops.extend((0..am.len()).map(|i| Op::Del(pre + i)));
        ops.extend((0..bm.len()).map(|j| Op::Ins(pre + j)));
    } else {
        ops.extend(myers(am, bm).into_iter().map(|o| match o {
            Op::Same(i, j) => Op::Same(i + pre, j + pre),
            Op::Del(i) => Op::Del(i + pre),
            Op::Ins(j) => Op::Ins(j + pre),
        }));
    }
    ops.extend((0..suf).map(|k| Op::Same(a.len() - suf + k, b.len() - suf + k)));
    ops
}

fn myers(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    if max == 0 {
        return vec![];
    }
    let off = max as isize;
    let mut v = vec![0isize; 2 * max + 2];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    'outer: for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
                v[idx + 1]
            } else {
                v[idx - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                trace.push(v.clone());
                break 'outer;
            }
            k += 2;
        }
    }
    // Backtrack.
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..trace.len() - 1).rev() {
        let v = &trace[d];
        let d = d as isize;
        let k = x - y;
        let prev_k = if k == -d || (k != d && v[(k - 1 + off) as usize] < v[(k + 1 + off) as usize])
        {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(prev_k + off) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Same(x as usize, y as usize));
        }
        if x == prev_x {
            y -= 1;
            ops.push(Op::Ins(y as usize));
        } else {
            x -= 1;
            ops.push(Op::Del(x as usize));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push(Op::Same(x as usize, y as usize));
    }
    ops.reverse();
    ops
}

/// A unified diff of `old` → `new` with 3 lines of context, `--- a/path` / `+++ b/path`
/// headers. Empty when nothing changed.
pub fn unified(path: &str, old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = diff_lines(&a, &b);
    if ops.iter().all(|o| matches!(o, Op::Same(..))) {
        if old == new {
            return String::new();
        }
        // Only a trailing newline differs.
        return format!("--- a/{path}\n+++ b/{path}\n@@ end of file @@\n\\ trailing newline changed\n");
    }
    const CTX: usize = 3;
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, Op::Same(..)))
        .map(|(i, _)| i)
        .collect();
    let mut i = 0;
    while i < changed.len() {
        let start = changed[i].saturating_sub(CTX);
        let mut end = changed[i];
        let mut j = i;
        while j + 1 < changed.len() && changed[j + 1] <= end + 2 * CTX + 1 {
            j += 1;
            end = changed[j];
        }
        let end = (end + CTX + 1).min(ops.len());
        let slice = &ops[start..end];
        let (mut a0, mut b0) = (None, None);
        let (mut ac, mut bc) = (0, 0);
        for o in slice {
            match *o {
                Op::Same(x, y) => {
                    a0.get_or_insert(x);
                    b0.get_or_insert(y);
                    ac += 1;
                    bc += 1;
                }
                Op::Del(x) => {
                    a0.get_or_insert(x);
                    ac += 1;
                }
                Op::Ins(y) => {
                    b0.get_or_insert(y);
                    bc += 1;
                }
            }
        }
        let a_start = a0.map_or(0, |x| x + 1);
        let b_start = b0.map_or(0, |y| y + 1);
        out.push_str(&format!("@@ -{a_start},{ac} +{b_start},{bc} @@\n"));
        for o in slice {
            match *o {
                Op::Same(x, _) => {
                    out.push(' ');
                    out.push_str(a[x]);
                }
                Op::Del(x) => {
                    out.push('-');
                    out.push_str(a[x]);
                }
                Op::Ins(y) => {
                    out.push('+');
                    out.push_str(b[y]);
                }
            }
            out.push('\n');
        }
        i = j + 1;
    }
    out
}

/// The first `max` lines of a diff, with a note when cut.
pub fn clip(diff: &str, max: usize) -> String {
    let n = diff.lines().count();
    if n <= max {
        return diff.to_string();
    }
    let mut s: String = diff.lines().take(max).collect::<Vec<_>>().join("\n");
    s.push_str(&format!("\n[… {} more diff lines]", n - max));
    s
}

/// `(added, removed)` line counts of a unified diff.
pub fn stat(diff: &str) -> (usize, usize) {
    let mut a = 0;
    let mut r = 0;
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if l.starts_with('+') {
            a += 1;
        } else if l.starts_with('-') {
            r += 1;
        }
    }
    (a, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(a: &[&str], b: &[&str]) -> Vec<String> {
        diff_lines(a, b)
            .into_iter()
            .filter_map(|o| match o {
                Op::Same(i, _) => Some(a[i].to_string()),
                Op::Ins(j) => Some(b[j].to_string()),
                Op::Del(_) => None,
            })
            .collect()
    }

    #[test]
    fn myers_reconstructs_the_target() {
        let cases: &[(&[&str], &[&str])] = &[
            (&["a", "b", "c"], &["a", "x", "c"]),
            (&[], &["a"]),
            (&["a"], &[]),
            (&["a", "b", "c", "d"], &["d", "c", "b", "a"]),
            (&["x", "a", "b", "y"], &["a", "q", "b"]),
        ];
        for (a, b) in cases {
            assert_eq!(apply(a, b), b.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn unified_has_hunk_headers() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let new = "1\n2\n3\n4\nfive\n6\n7\n8\n9\n10\n";
        let d = unified("f.txt", old, new);
        assert!(d.starts_with("--- a/f.txt\n+++ b/f.txt\n@@ -2,7 +2,7 @@\n"), "{d}");
        assert!(d.contains("-5\n+five\n"));
        assert_eq!(stat(&d), (1, 1));
        assert_eq!(unified("f", "a\n", "a\n"), "");
        let created = unified("n", "", "x\ny\n");
        assert!(created.contains("@@ -0,0 +1,2 @@\n+x\n+y\n"), "{created}");
    }
}
