TODO:

1. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules
1. git blame view for a file, `B` will open a new window showing git blame?
   - Where: L on a file or folder in the local changes page or the details tabs switches the graph to only the commits that touched it. The graph title says which path, and Esc shows everything again. This reuses how the "branch only" mode and ref comparisons already reload the graph.
   - How: git's log -- <path> with --parents, so git rewrites each commit's parents to the nearest commit that also touched the path, and the lines stay connected.
   - Risk: squash, drop, amend and rebase from the graph look up commit parents in the loaded graph, and in a filtered graph those parents are wrong. Running them there could rewrite the wrong commits. I'd turn them off while the filter is on, with a message.
   - Not planned: following renames (--follow). git only supports it for a single file, and it doesn't work well with a graph.
1. Simplify
1. Code Review
1. Github workflow to prepare for release
1. See if the profile pics can be rendered alongside the name in commit graph
