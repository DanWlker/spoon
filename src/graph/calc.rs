use std::ops::Range;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::git::{Commit, CommitHash, Ref, Repository};

type CommitPosMap<'a> = FxHashMap<&'a CommitHash, (usize, usize)>;

#[derive(Debug)]
pub struct Graph<'a> {
    pub commits: Vec<&'a Commit>,
    pub commit_pos_map: CommitPosMap<'a>,
    pub edges: GraphEdges,
    pub max_pos_x: usize,
    pub warning: Option<String>,
    // the merge commits shown, those with two parents or more loaded, see `calc_graph`
    pub merges: FxHashMap<&'a CommitHash, Fold>,
}

// Whether a merge commit shows the commits it brought in, see `calc_graph`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fold {
    Folded,
    Unfolded,
}

// The parents and children of the commits shown: a folded merge has its first parent only, and
// the commits it brought in are left out, even those a branch or tag points at, unless a commit
// shown still reaches them; the checked-out commit always stays.
struct Links<'a, 'f> {
    repository: &'a Repository,
    folded: &'f FxHashSet<CommitHash>,
    // None for all
    shown: Option<FxHashSet<&'a CommitHash>>,
}

impl<'a, 'f> Links<'a, 'f> {
    fn new(repository: &'a Repository, folded: &'f FxHashSet<CommitHash>) -> Self {
        let mut links = Links {
            repository,
            folded,
            shown: None,
        };
        if folded.is_empty() {
            return links;
        }
        // from the commits with no children and the checked-out one, which stay shown
        let head = repository.head_commit();
        let mut stack: Vec<&CommitHash> = repository
            .all_commits()
            .into_iter()
            .map(|c| &c.commit_hash)
            .filter(|h| repository.children_hash(h).is_empty() || head.as_ref() == Some(*h))
            .collect();
        let mut shown = FxHashSet::default();
        while let Some(hash) = stack.pop() {
            if repository.commit(hash).is_some() && shown.insert(hash) {
                stack.extend(links.parents_hash(hash));
            }
        }
        links.shown = Some(shown);
        links
    }

    fn is_shown(&self, hash: &CommitHash) -> bool {
        self.shown.as_ref().is_none_or(|shown| shown.contains(hash))
    }

    fn parents_hash(&self, hash: &CommitHash) -> Vec<&'a CommitHash> {
        let mut parents = self.repository.parents_hash(hash);
        if self.folded.contains(hash) {
            parents.truncate(1);
        }
        parents
    }

    fn children_hash(&self, hash: &CommitHash) -> Vec<&'a CommitHash> {
        let mut children = self.repository.children_hash(hash);
        children.retain(|c| self.is_shown(c) && self.parents_hash(c).contains(&hash));
        children
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    pub edge_type: EdgeType,
    pub pos_x: usize,
    pub associated_line_pos_x: usize,
}

impl Edge {
    pub fn new(edge_type: EdgeType, pos_x: usize, line_pos_x: usize) -> Self {
        Self {
            edge_type,
            pos_x,
            associated_line_pos_x: line_pos_x,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeType {
    Vertical,    // │
    Horizontal,  // ─
    Up,          // ╵
    Down,        // ╷
    Left,        // ╴
    Right,       // ╶
    RightTop,    // ╮
    RightBottom, // ╯
    LeftTop,     // ╭
    LeftBottom,  // ╰
}

impl EdgeType {
    pub fn is_vertically_related(&self) -> bool {
        matches!(self, EdgeType::Vertical | EdgeType::Up | EdgeType::Down)
    }
}

// The `folded` merges hiding `target`, which unfolded show it: those it was brought in by; none when
// it is shown, or not loaded.
pub fn merges_hiding(
    repository: &Repository,
    folded: &FxHashSet<CommitHash>,
    target: &CommitHash,
) -> Vec<CommitHash> {
    if repository.commit(target).is_none() || Links::new(repository, folded).is_shown(target) {
        return vec![];
    }
    // the commits reaching it: it and its descendants
    let mut reaching = FxHashSet::default();
    let mut stack = vec![target];
    while let Some(hash) = stack.pop() {
        if reaching.insert(hash) {
            stack.extend(repository.children_hash(hash));
        }
    }
    folded
        .iter()
        .filter(|merge| {
            let parents = repository.parents_hash(merge);
            parents.iter().skip(1).any(|p| reaching.contains(p))
        })
        .cloned()
        .collect()
}

// The graph of the commits loaded, the `folded` merges without the commits they brought in.
pub fn calc_graph<'a>(
    repository: &'a Repository,
    primary_branch: Option<&str>,
    folded: &FxHashSet<CommitHash>,
) -> Graph<'a> {
    Graph::of(
        repository,
        GraphLayout::calc(repository, primary_branch, folded),
    )
}

// The graph laid out, owning what it holds, so a repository reloaded on another thread is laid out
// there too; `Graph::of` makes it the graph of that repository.
#[derive(Debug)]
pub struct GraphLayout {
    // the commits shown by row, with the column of each
    rows: Vec<(CommitHash, usize)>,
    edges: GraphEdges,
    max_pos_x: usize,
    warning: Option<String>,
    // the rows of the merge commits shown
    merges: Vec<(usize, Fold)>,
    // the merges it was laid out with folded
    pub folded: FxHashSet<CommitHash>,
}

impl GraphLayout {
    pub fn calc(
        repository: &Repository,
        primary_branch: Option<&str>,
        folded: &FxHashSet<CommitHash>,
    ) -> Self {
        let links = Links::new(repository, folded);
        let mut commits = repository.all_commits();
        commits.retain(|c| links.is_shown(&c.commit_hash));

        let (commit_pos_map, warning) = calc_commit_positions(&commits, &links, primary_branch);
        let (edges, max_pos_x) = calc_edges(&commit_pos_map, &commits, &links);
        // a stash has parents not loaded
        let merges = commits
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let parents = repository.parents_hash(&c.commit_hash);
                parents
                    .iter()
                    .filter(|p| repository.commit(p).is_some())
                    .count()
                    > 1
            })
            .map(|(row, c)| {
                let fold = if folded.contains(&c.commit_hash) {
                    Fold::Folded
                } else {
                    Fold::Unfolded
                };
                (row, fold)
            })
            .collect();
        let rows = commits
            .iter()
            .map(|c| (c.commit_hash.clone(), commit_pos_map[&c.commit_hash].0))
            .collect();
        GraphLayout {
            rows,
            edges,
            max_pos_x,
            warning,
            merges,
            folded: folded.clone(),
        }
    }
}

impl<'a> Graph<'a> {
    // The graph of `repository` as `layout`, laid out from it.
    pub fn of(repository: &'a Repository, layout: GraphLayout) -> Self {
        let commits: Vec<&Commit> = layout
            .rows
            .iter()
            .map(|(hash, _)| {
                repository
                    .commit(hash)
                    .expect("laid out from the repository")
            })
            .collect();
        let commit_pos_map = commits
            .iter()
            .zip(&layout.rows)
            .enumerate()
            .map(|(pos_y, (commit, (_, pos_x)))| (&commit.commit_hash, (*pos_x, pos_y)))
            .collect();
        let merges = layout
            .merges
            .into_iter()
            .map(|(row, fold)| (&commits[row].commit_hash, fold))
            .collect();
        Graph {
            commits,
            commit_pos_map,
            edges: layout.edges,
            max_pos_x: layout.max_pos_x,
            warning: layout.warning,
            merges,
        }
    }
}

fn find_primary_branch_tip<'a>(
    repository: &'a Repository,
    primary_branch: &str,
) -> Option<&'a CommitHash> {
    if primary_branch.is_empty() {
        return None;
    }

    for r in repository.all_refs() {
        if let Ref::Branch { name, target } = r {
            if name == primary_branch {
                return Some(target);
            }
        }
    }

    None
}

fn primary_branch_spine<'a>(
    commits: &[&'a Commit],
    links: &Links<'a, '_>,
    primary_branch: Option<&str>,
) -> (FxHashSet<&'a CommitHash>, Option<String>) {
    let Some(branch) = primary_branch else {
        return (FxHashSet::default(), None);
    };

    let Some(tip) = find_primary_branch_tip(links.repository, branch) else {
        let msg = format!("Primary branch '{branch}' could not be resolved");
        return (FxHashSet::default(), Some(msg));
    };

    if !commits.iter().any(|c| &c.commit_hash == tip) {
        let msg = format!("Primary branch '{branch}' tip is not included in the loaded commits");
        return (FxHashSet::default(), Some(msg));
    }

    let mut spine = FxHashSet::default();
    let mut current = Some(tip);

    while let Some(hash) = current {
        spine.insert(hash);
        let parents = links.parents_hash(hash);
        current = parents.first().copied();
    }

    (spine, None)
}

fn calc_commit_positions<'a>(
    commits: &[&'a Commit],
    links: &Links<'a, '_>,
    primary_branch: Option<&str>,
) -> (CommitPosMap<'a>, Option<String>) {
    let mut commit_pos_map: CommitPosMap = FxHashMap::default();
    let mut commit_line_state: Vec<Option<&CommitHash>> = Vec::new();
    let mut line_last_occupied_pos_y: Vec<usize> = Vec::new();
    let (primary_spine, warning) = primary_branch_spine(commits, links, primary_branch);
    let has_visible_primary = !primary_spine.is_empty();
    let min_col = if has_visible_primary { 1 } else { 0 };

    for (pos_y, commit) in commits.iter().enumerate() {
        let is_primary = primary_spine.contains(&commit.commit_hash);
        let filtered_children_hash = filtered_children_hash(commit, links);
        if filtered_children_hash.is_empty() {
            let merge_child_pos_y = links
                .children_hash(&commit.commit_hash)
                .into_iter()
                .map(|hash| commit_pos_map[hash].1)
                .min();

            let pos_x = if is_primary {
                // For a primary spine commit with no first-parent children (i.e. tip of primary branch),
                // verify that column 0 is vacant and doesn't overlap an active merge line before assigning it.
                let col_0_vacant = commit_line_state.first().is_none_or(|c| c.is_none());
                let col_0_no_overlap = merge_child_pos_y.is_none_or(|child_y| {
                    line_last_occupied_pos_y.first().copied().unwrap_or(0) <= child_y
                });
                if col_0_vacant && col_0_no_overlap {
                    0
                } else {
                    get_first_vacant_line(
                        &commit_line_state,
                        &line_last_occupied_pos_y,
                        merge_child_pos_y,
                        min_col,
                    )
                }
            } else {
                get_first_vacant_line(
                    &commit_line_state,
                    &line_last_occupied_pos_y,
                    merge_child_pos_y,
                    min_col,
                )
            };
            add_commit_line(commit, &mut commit_line_state, pos_x);
            commit_pos_map.insert(&commit.commit_hash, (pos_x, pos_y));
            occupy_line(&mut line_last_occupied_pos_y, pos_x, pos_y);
        } else {
            let mut pos_x =
                update_commit_line(commit, &mut commit_line_state, &filtered_children_hash);

            // If this is a primary spine commit and update_commit_line placed it in a column > 0
            // (e.g. because first-parent children were on non-zero columns), properly relocate
            // the state line to column 0 if vacant to keep commit_line_state and commit_pos_map synchronized.
            if is_primary && pos_x != 0 && commit_line_state.first().is_none_or(|c| c.is_none()) {
                commit_line_state[pos_x] = None;
                if commit_line_state.is_empty() {
                    commit_line_state.push(Some(&commit.commit_hash));
                } else {
                    commit_line_state[0] = Some(&commit.commit_hash);
                }
                pos_x = 0;
            }

            commit_pos_map.insert(&commit.commit_hash, (pos_x, pos_y));
            for child_hash in filtered_children_hash {
                let child_pos_x = commit_pos_map[child_hash].0;
                occupy_line(&mut line_last_occupied_pos_y, child_pos_x, pos_y);
            }
        }
    }

    (commit_pos_map, warning)
}

fn filtered_children_hash<'a>(commit: &'a Commit, links: &Links<'a, '_>) -> Vec<&'a CommitHash> {
    links
        .children_hash(&commit.commit_hash)
        .into_iter()
        .filter(|child_hash| {
            let child_parents_hash = links.parents_hash(child_hash);
            !child_parents_hash.is_empty() && *child_parents_hash[0] == commit.commit_hash
        })
        .collect()
}

fn get_first_vacant_line(
    commit_line_state: &[Option<&CommitHash>],
    line_last_occupied_pos_y: &[usize],
    merge_child_pos_y: Option<usize>,
    min_col: usize,
) -> usize {
    commit_line_state
        .iter()
        .enumerate()
        .skip(min_col)
        .position(|(pos_x, commit)| {
            // A line can be vacant in the current state while still containing an edge above
            // this row. Reusing it for a merge edge that reaches above that edge would make two
            // unrelated branches appear to be connected.
            commit.is_none()
                && merge_child_pos_y.is_none_or(|merge_child_pos_y| {
                    line_last_occupied_pos_y.get(pos_x).copied().unwrap_or(0) <= merge_child_pos_y
                })
        })
        .map(|pos_x| pos_x + min_col)
        .unwrap_or_else(|| commit_line_state.len().max(min_col))
}

fn occupy_line(line_last_occupied_pos_y: &mut Vec<usize>, pos_x: usize, pos_y: usize) {
    if line_last_occupied_pos_y.len() <= pos_x {
        line_last_occupied_pos_y.resize(pos_x + 1, 0);
    }
    line_last_occupied_pos_y[pos_x] = pos_y;
}

fn add_commit_line<'a>(
    commit: &'a Commit,
    commit_line_state: &mut Vec<Option<&'a CommitHash>>,
    pos_x: usize,
) {
    if commit_line_state.len() <= pos_x {
        commit_line_state.resize(pos_x + 1, None);
    }
    commit_line_state[pos_x] = Some(&commit.commit_hash);
}

fn update_commit_line<'a>(
    commit: &'a Commit,
    commit_line_state: &mut [Option<&'a CommitHash>],
    target_commit_hashes: &[&CommitHash],
) -> usize {
    if commit_line_state.is_empty() {
        return 0;
    }
    let mut min_pos_x = commit_line_state.len().saturating_sub(1);
    for target_hash in target_commit_hashes {
        for (pos_x, commit_hash) in commit_line_state.iter().enumerate() {
            if let Some(hash) = commit_hash {
                if hash == target_hash {
                    commit_line_state[pos_x] = None;
                    if min_pos_x > pos_x {
                        min_pos_x = pos_x;
                    }
                    break;
                }
            }
        }
    }
    commit_line_state[min_pos_x] = Some(&commit.commit_hash);
    min_pos_x
}

// The rows of chunk of rows the vertical runs are found by, see `GraphEdges`.
const CHUNK: usize = 64;

// A vertical line of a lane over consecutive rows, from a commit up to its child: one run rather
// than an edge per row, which in a big history are millions.
#[derive(Debug, Clone)]
struct VerticalRun {
    rows: Range<usize>,
    pos_x: usize,
    line_pos_x: usize,
    // the row of the commit the line goes up from, as merge lines detour around other commits'
    // lines
    parent: usize,
}

// The edges of the rows of the graph: those within a row, and the vertical runs crossing rows,
// found by the chunks of `CHUNK` rows they cross. See `at` for those of a row.
#[derive(Debug, Default)]
pub struct GraphEdges {
    rows: Vec<Vec<Edge>>,
    runs: Vec<VerticalRun>,
    // the runs crossing each chunk of rows, by their index
    chunks: Vec<Vec<usize>>,
}

impl GraphEdges {
    fn new(rows: usize) -> Self {
        GraphEdges {
            rows: vec![Vec::new(); rows],
            runs: Vec::new(),
            chunks: vec![Vec::new(); rows.div_ceil(CHUNK)],
        }
    }

    // An edge within `row`; a vertical one over rows is a run, see `push_run`.
    fn push(&mut self, row: usize, edge: Edge) {
        self.rows[row].push(edge);
    }

    // A vertical line of `pos_x` over `rows`.
    fn push_run(&mut self, rows: Range<usize>, pos_x: usize, line_pos_x: usize, parent: usize) {
        if rows.is_empty() {
            return;
        }
        let index = self.runs.len();
        for chunk in rows.start / CHUNK..=(rows.end - 1) / CHUNK {
            self.chunks[chunk].push(index);
        }
        self.runs.push(VerticalRun {
            rows,
            pos_x,
            line_pos_x,
            parent,
        });
    }

    // The runs crossing some of `rows`, each once.
    fn runs_crossing(&self, rows: Range<usize>) -> impl Iterator<Item = &VerticalRun> + '_ {
        let chunks = match rows.is_empty() {
            true => 0..0,
            false => rows.start / CHUNK..(rows.end - 1) / CHUNK + 1,
        };
        chunks.flat_map(move |chunk| {
            let rows = rows.clone();
            self.chunks[chunk].iter().filter_map(move |&index| {
                let run = &self.runs[index];
                let start = run.rows.start.max(rows.start);
                let crosses = start < run.rows.end.min(rows.end);
                // in the first chunk the run and `rows` share, not again in the next ones
                (crosses && start / CHUNK == chunk).then_some(run)
            })
        })
    }

    // The edges of `row`, sorted and without repeats, to draw it.
    pub fn at(&self, row: usize) -> Vec<Edge> {
        let runs = self
            .runs_crossing(row..row + 1)
            .map(|run| Edge::new(EdgeType::Vertical, run.pos_x, run.line_pos_x));
        let mut edges: Vec<Edge> = self.rows[row].iter().copied().chain(runs).collect();
        edges.sort_by_key(|e| (e.associated_line_pos_x, e.pos_x, e.edge_type));
        edges.dedup();
        edges
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

fn calc_edges(
    commit_pos_map: &CommitPosMap,
    commits: &[&Commit],
    links: &Links,
) -> (GraphEdges, usize) {
    let mut max_pos_x = 0;
    let mut edges = GraphEdges::new(commits.len());

    for commit in commits {
        let (pos_x, pos_y) = commit_pos_map[&commit.commit_hash];
        let hash = &commit.commit_hash;

        for child_hash in links.children_hash(hash) {
            let (child_pos_x, child_pos_y) = commit_pos_map[child_hash];

            if pos_x == child_pos_x {
                // commit
                edges.push(pos_y, Edge::new(EdgeType::Up, pos_x, pos_x));
                edges.push_run((child_pos_y + 1)..pos_y, pos_x, pos_x, pos_y);
                edges.push(child_pos_y, Edge::new(EdgeType::Down, pos_x, pos_x));
            } else {
                let child_first_parent_hash = &commits[child_pos_y].parent_commit_hashes[0];
                if *child_first_parent_hash == *hash {
                    // branch
                    if pos_x < child_pos_x {
                        edges.push(pos_y, Edge::new(EdgeType::Right, pos_x, child_pos_x));
                        for x in (pos_x + 1)..child_pos_x {
                            edges.push(pos_y, Edge::new(EdgeType::Horizontal, x, child_pos_x));
                        }
                        edges.push(
                            pos_y,
                            Edge::new(EdgeType::RightBottom, child_pos_x, child_pos_x),
                        );
                    } else {
                        edges.push(pos_y, Edge::new(EdgeType::Left, pos_x, child_pos_x));
                        for x in (child_pos_x + 1)..pos_x {
                            edges.push(pos_y, Edge::new(EdgeType::Horizontal, x, child_pos_x));
                        }
                        edges.push(
                            pos_y,
                            Edge::new(EdgeType::LeftBottom, child_pos_x, child_pos_x),
                        );
                    }
                    edges.push_run((child_pos_y + 1)..pos_y, child_pos_x, child_pos_x, pos_y);
                    edges.push(
                        child_pos_y,
                        Edge::new(EdgeType::Down, child_pos_x, child_pos_x),
                    );
                } else {
                    // merge
                    // skip
                }
            }
        }

        if max_pos_x < pos_x {
            max_pos_x = pos_x;
        }

        // draw down edge if has parent but parent not in the graph (when max_count is set)
        if !commit.parent_commit_hashes.is_empty()
            && links
                .repository
                .commit(&commit.parent_commit_hashes[0])
                .is_none()
        {
            edges.push(pos_y, Edge::new(EdgeType::Down, pos_x, pos_x));
            edges.push_run((pos_y + 1)..commits.len(), pos_x, pos_x, pos_y);
        }
    }

    // the lane of the commit of each row
    let commit_pos_xs: Vec<usize> = commits
        .iter()
        .map(|c| commit_pos_map[&c.commit_hash].0)
        .collect();
    for commit in commits {
        let (pos_x, pos_y) = commit_pos_map[&commit.commit_hash];
        let hash = &commit.commit_hash;

        for child_hash in links.children_hash(hash) {
            let (child_pos_x, child_pos_y) = commit_pos_map[child_hash];

            if pos_x == child_pos_x {
                // commit
                // skip
                continue;
            }
            let child_first_parent_hash = &commits[child_pos_y].parent_commit_hashes[0];
            if *child_first_parent_hash == *hash {
                // branch
                // skip
                continue;
            }
            // merge: straight up its lane, unless a commit or another commit's line is in the
            // way, then detouring right of them
            let between = (child_pos_y + 1)..pos_y;
            let in_the_way = between.clone().any(|y| commit_pos_xs[y] == pos_x)
                || edges
                    .runs_crossing(between.clone())
                    .any(|run| run.pos_x == pos_x && run.parent != pos_y);
            let mut overlap = false;
            let mut new_pos_x = pos_x;
            if in_the_way {
                // row by row, right of the commit, then of the lines of other commits there right
                // of where it would go: the rightmost of them, the lines present on each row
                // tracked from where they start to where they end
                let mut changes: Vec<(usize, bool, usize)> = edges
                    .runs_crossing(between.clone())
                    .filter(|run| run.parent != pos_y)
                    .flat_map(|run| {
                        [
                            (run.rows.start.max(between.start), true, run.pos_x),
                            (run.rows.end, false, run.pos_x),
                        ]
                    })
                    .collect();
                changes.sort_unstable();
                let mut changes = changes.into_iter().peekable();
                let mut present: std::collections::BTreeMap<usize, usize> = Default::default();
                for y in between.clone() {
                    while let Some((_, starts, x)) = changes.next_if(|(row, ..)| *row <= y) {
                        if starts {
                            *present.entry(x).or_default() += 1;
                        } else if let Some(count) = present.get_mut(&x) {
                            *count -= 1;
                            if *count == 0 {
                                present.remove(&x);
                            }
                        }
                    }
                    if commit_pos_xs[y] == new_pos_x {
                        overlap = true;
                        new_pos_x += 1;
                    }
                    if let Some((&x, _)) =
                        present.last_key_value().filter(|(x, _)| **x >= new_pos_x)
                    {
                        overlap = true;
                        new_pos_x = x + 1;
                    }
                }
            }

            if overlap {
                // detour
                edges.push(pos_y, Edge::new(EdgeType::Right, pos_x, pos_x));
                for x in (pos_x + 1)..new_pos_x {
                    edges.push(pos_y, Edge::new(EdgeType::Horizontal, x, pos_x));
                }
                edges.push(pos_y, Edge::new(EdgeType::RightBottom, new_pos_x, pos_x));
                edges.push_run(between, new_pos_x, pos_x, pos_y);
                edges.push(child_pos_y, Edge::new(EdgeType::RightTop, new_pos_x, pos_x));
                for x in (child_pos_x + 1)..new_pos_x {
                    edges.push(child_pos_y, Edge::new(EdgeType::Horizontal, x, pos_x));
                }
                edges.push(child_pos_y, Edge::new(EdgeType::Right, child_pos_x, pos_x));

                if max_pos_x < new_pos_x {
                    max_pos_x = new_pos_x;
                }
            } else {
                edges.push(pos_y, Edge::new(EdgeType::Up, pos_x, pos_x));
                edges.push_run(between, pos_x, pos_x, pos_y);
                if pos_x < child_pos_x {
                    edges.push(child_pos_y, Edge::new(EdgeType::LeftTop, pos_x, pos_x));
                    for x in (pos_x + 1)..child_pos_x {
                        edges.push(child_pos_y, Edge::new(EdgeType::Horizontal, x, pos_x));
                    }
                    edges.push(child_pos_y, Edge::new(EdgeType::Left, child_pos_x, pos_x));
                } else {
                    edges.push(child_pos_y, Edge::new(EdgeType::RightTop, pos_x, pos_x));
                    for x in (child_pos_x + 1)..pos_x {
                        edges.push(child_pos_y, Edge::new(EdgeType::Horizontal, x, pos_x));
                    }
                    edges.push(child_pos_y, Edge::new(EdgeType::Right, child_pos_x, pos_x));
                }
            }
        }

        if max_pos_x < pos_x {
            max_pos_x = pos_x;
        }
    }

    (edges, max_pos_x)
}
