//! Graph helpers: deps[i] = stages that must finish before stage i.

/// Kahn topological order (stable: lowest index first among ready nodes). None on a cycle.
pub fn topo_order(n: usize, deps: &[Vec<usize>]) -> Option<Vec<usize>> {
    let mut indeg = vec![0usize; n];
    let mut users: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, ds) in deps.iter().enumerate() {
        for &d in ds {
            indeg[i] += 1;
            users[d].push(i);
        }
    }
    let mut ready: std::collections::BTreeSet<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut out = Vec::with_capacity(n);
    while let Some(&i) = ready.iter().next() {
        ready.remove(&i);
        out.push(i);
        for &u in &users[i] {
            indeg[u] -= 1;
            if indeg[u] == 0 {
                ready.insert(u);
            }
        }
    }
    if out.len() == n { Some(out) } else { None }
}

/// all stages reachable from `from` following dependants (users), including `from`.
pub fn descendants(n: usize, deps: &[Vec<usize>], from: usize) -> Vec<bool> {
    let mut users: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, ds) in deps.iter().enumerate() {
        for &d in ds {
            users[d].push(i);
        }
    }
    let mut seen = vec![false; n];
    let mut stack = vec![from];
    while let Some(i) = stack.pop() {
        if seen[i] {
            continue;
        }
        seen[i] = true;
        stack.extend(users[i].iter().copied());
    }
    seen
}

/// is `a` an ancestor of `b` (a must finish before b) under deps?
pub fn is_ancestor(deps: &[Vec<usize>], a: usize, b: usize) -> bool {
    let mut seen = vec![false; deps.len()];
    let mut stack = vec![b];
    while let Some(i) = stack.pop() {
        for &d in &deps[i] {
            if d == a {
                return true;
            }
            if !seen[d] {
                seen[d] = true;
                stack.push(d);
            }
        }
    }
    false
}
