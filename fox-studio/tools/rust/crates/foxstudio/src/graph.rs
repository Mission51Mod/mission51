//! Layered layout of the stage graph (left to right): a stage's column is its longest dependency chain; rows within
//! a column are ordered by the mean row of their dependencies (a few barycentre sweeps), which keeps edges short and
//! mostly uncrossed for pipeline-shaped graphs. Pure and deterministic (tested).

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// column (layer) of each node
    pub layer: Vec<usize>,
    /// row of each node within its layer
    pub row: Vec<usize>,
    pub layers: usize,
    /// the tallest layer's node count
    pub max_rows: usize,
}

/// `deps[i]` = nodes that must come before node i. Cycles are tolerated (back edges ignored).
pub fn layout(deps: &[Vec<usize>]) -> Layout {
    let n = deps.len();
    // longest path layering, by DFS with cycle guard
    let mut layer = vec![usize::MAX; n];
    let mut visiting = vec![false; n];
    fn depth(i: usize, deps: &[Vec<usize>], layer: &mut [usize], visiting: &mut [bool]) -> usize {
        if layer[i] != usize::MAX {
            return layer[i];
        }
        visiting[i] = true;
        let mut d = 0;
        for &p in &deps[i] {
            if p < deps.len() && !visiting[p] {
                d = d.max(depth(p, deps, layer, visiting) + 1);
            }
        }
        visiting[i] = false;
        layer[i] = d;
        d
    }
    for i in 0..n {
        depth(i, deps, &mut layer, &mut visiting);
    }
    let layers = layer.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    // initial order: node index (graph-file / topological order) within each layer
    let mut cols: Vec<Vec<usize>> = vec![vec![]; layers];
    for i in 0..n {
        cols[layer[i]].push(i);
    }
    let mut row = vec![0usize; n];
    let assign = |cols: &Vec<Vec<usize>>, row: &mut Vec<usize>| {
        for c in cols {
            for (r, &i) in c.iter().enumerate() {
                row[i] = r;
            }
        }
    };
    assign(&cols, &mut row);
    // users (for the backward sweep)
    let mut users: Vec<Vec<usize>> = vec![vec![]; n];
    for (i, ds) in deps.iter().enumerate() {
        for &d in ds {
            if d < n {
                users[d].push(i);
            }
        }
    }
    for sweep in 0..6 {
        let forward = sweep % 2 == 0;
        let order: Vec<usize> = if forward { (1..layers).collect() } else { (0..layers.saturating_sub(1)).rev().collect() };
        for l in order {
            let mut keyed: Vec<(f64, usize, usize)> = cols[l]
                .iter()
                .map(|&i| {
                    let nb: Vec<usize> = if forward { deps[i].clone() } else { users[i].clone() };
                    let nb: Vec<usize> = nb.into_iter().filter(|&j| j < n && layer[j] != layer[i]).collect();
                    let key = if nb.is_empty() { row[i] as f64 } else { nb.iter().map(|&j| row[j] as f64).sum::<f64>() / nb.len() as f64 };
                    (key, row[i], i)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
            cols[l] = keyed.into_iter().map(|k| k.2).collect();
            for (r, &i) in cols[l].iter().enumerate() {
                row[i] = r;
            }
        }
    }
    let max_rows = cols.iter().map(|c| c.len()).max().unwrap_or(0);
    Layout { layer, row, layers, max_rows }
}

/// every node upstream (ancestors) / downstream (descendants) of `from`, excluding it
pub fn reach(deps: &[Vec<usize>], from: usize, upstream: bool) -> Vec<bool> {
    let n = deps.len();
    let mut users: Vec<Vec<usize>> = vec![vec![]; n];
    for (i, ds) in deps.iter().enumerate() {
        for &d in ds {
            if d < n {
                users[d].push(i);
            }
        }
    }
    let mut seen = vec![false; n];
    let mut stack: Vec<usize> = if upstream { deps[from].clone() } else { users[from].clone() };
    while let Some(i) = stack.pop() {
        if i >= n || seen[i] {
            continue;
        }
        seen[i] = true;
        stack.extend(if upstream { deps[i].iter().copied() } else { users[i].iter().copied() }.collect::<Vec<_>>());
    }
    seen[from] = false;
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_follow_dependencies() {
        // 0 -> 1 -> 3, 0 -> 2 -> 3, 4 alone
        let deps = vec![vec![], vec![0], vec![0], vec![1, 2], vec![]];
        let l = layout(&deps);
        assert_eq!(l.layer, vec![0, 1, 1, 2, 0]);
        assert_eq!(l.layers, 3);
        for (i, ds) in deps.iter().enumerate() {
            for &d in ds {
                assert!(l.layer[d] < l.layer[i]);
            }
        }
        // no two nodes share a slot
        let mut slots: Vec<(usize, usize)> = (0..deps.len()).map(|i| (l.layer[i], l.row[i])).collect();
        slots.sort();
        slots.dedup();
        assert_eq!(slots.len(), deps.len());
        assert_eq!(l.max_rows, 2);
    }

    #[test]
    fn cycles_do_not_hang() {
        let deps = vec![vec![1], vec![0], vec![1]];
        let l = layout(&deps);
        assert_eq!(l.layer.len(), 3);
    }

    #[test]
    fn deterministic() {
        let deps: Vec<Vec<usize>> = (0..40).map(|i| if i == 0 { vec![] } else { vec![(i * 7) % i, i / 2] }).collect();
        assert_eq!(layout(&deps), layout(&deps));
    }

    #[test]
    fn reach_up_and_down() {
        let deps = vec![vec![], vec![0], vec![1], vec![]];
        assert_eq!(reach(&deps, 1, true), vec![true, false, false, false]);
        assert_eq!(reach(&deps, 1, false), vec![false, false, true, false]);
        assert_eq!(reach(&deps, 3, true), vec![false; 4]);
    }
}
