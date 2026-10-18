//! A small unified diff for migrate dry-run output.

/// Unified diff of `old` → `new` named `name`.
#[must_use]
pub fn unified_diff(old: &str, new: &str, name: &str) -> String {
    let before: Vec<&str> = old.lines().collect();
    let after: Vec<&str> = new.lines().collect();
    if before == after {
        return format!("--- {name}\n+++ {name}\n");
    }
    let mut out = format!("--- {name}\n+++ {name}\n");
    let edits = edits(&before, &after);
    write_hunks(&mut out, &before, &after, &edits);
    out
}

#[derive(Clone, Copy)]
enum Edit {
    Keep,
    Delete,
    Insert,
}

fn edits(before: &[&str], after: &[&str]) -> Vec<(Edit, usize, usize)> {
    let pairs = lcs(before, after);
    let mut edits = Vec::new();
    let (mut i, mut j, mut p) = (0, 0, 0);
    while i < before.len() || j < after.len() {
        if p < pairs.len() && i == pairs[p].0 && j == pairs[p].1 {
            edits.push((Edit::Keep, i, j));
            i += 1;
            j += 1;
            p += 1;
        } else if p < pairs.len() && i < pairs[p].0 {
            edits.push((Edit::Delete, i, j));
            i += 1;
        } else if j < after.len() && (p == pairs.len() || j < pairs[p].1) {
            edits.push((Edit::Insert, i, j));
            j += 1;
        } else {
            break;
        }
    }
    edits
}

fn lcs(before: &[&str], after: &[&str]) -> Vec<(usize, usize)> {
    let (n, m) = (before.len(), after.len());
    let mut table = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            table[i + 1][j + 1] = if before[i] == after[j] {
                table[i][j] + 1
            } else {
                table[i][j + 1].max(table[i + 1][j])
            };
        }
    }
    walk_lcs(&table, n, m)
}

fn walk_lcs(table: &[Vec<usize>], mut i: usize, mut j: usize) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    while i > 0 && j > 0 {
        if table[i][j] == table[i - 1][j] {
            i -= 1;
        } else if table[i][j] == table[i][j - 1] {
            j -= 1;
        } else {
            i -= 1;
            j -= 1;
            pairs.push((i, j));
        }
    }
    pairs.reverse();
    pairs
}

fn write_hunks(out: &mut String, before: &[&str], after: &[&str], edits: &[(Edit, usize, usize)]) {
    let mut at = 0;
    while at < edits.len() {
        if matches!(edits[at].0, Edit::Keep) {
            at += 1;
            continue;
        }
        let start = at.saturating_sub(3);
        let mut end = at;
        while end < edits.len() {
            if !matches!(edits[end].0, Edit::Keep) {
                end += 1;
                continue;
            }
            let keep = edits[end..]
                .iter()
                .take_while(|edit| matches!(edit.0, Edit::Keep))
                .count();
            if keep >= 6 || end + keep == edits.len() {
                end += keep.min(3);
                break;
            }
            end += keep;
        }
        emit_hunk(out, before, after, &edits[start..end]);
        at = end;
    }
}

fn emit_hunk(out: &mut String, before: &[&str], after: &[&str], hunk: &[(Edit, usize, usize)]) {
    let old_start = hunk.first().map_or(1, |edit| edit.1 + 1);
    let new_start = hunk.first().map_or(1, |edit| edit.2 + 1);
    let old_len = hunk
        .iter()
        .filter(|edit| !matches!(edit.0, Edit::Insert))
        .count();
    let new_len = hunk
        .iter()
        .filter(|edit| !matches!(edit.0, Edit::Delete))
        .count();
    push_line(
        out,
        &format!("@@ -{old_start},{old_len} +{new_start},{new_len} @@"),
    );
    for (kind, i, j) in hunk {
        match kind {
            Edit::Keep => push_line(out, &format!(" {}", before[*i])),
            Edit::Delete => push_line(out, &format!("-{}", before[*i])),
            Edit::Insert => push_line(out, &format!("+{}", after[*j])),
        }
    }
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}
