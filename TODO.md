TODO:

1. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules
1. Move commits (do we really need this?)...
1. draw an architecture diagram showing which calls what, split by call layers. I want each file to list their functions and a short description of what it does,
1. Interactive rebase, switch refresh to Ctrl-R, use shift r for interactive rebase
1. when im diffing with my current branch, it should still dim the commits not on the current branch
1. make a config to determine whether to focus on the relevant branch/tag/stash on the commit graph when hovering. by default it should not auto do that. currently it is. my wording might be off so double confirm with me
1. space characters in branch names should automatically change to -, the user should see this as they are typing create new branch
1. instead of `nf-fa-cloud-upload` use `nf-cod-debug_connected` on the commits graph page branches that have been pushed and are in sync with remote
