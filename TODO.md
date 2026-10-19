TODO:

1. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules
1. Move commits (do we really need this?)...
1. draw an architecture diagram showing which calls what, split by call layers. I want each file to list their functions and a short description of what it does,
1. `+` and `-` on diff panel when viewing a single file only to show more around the context of the change. since this
   is changing, can hunks "select" only the lines with changes.
1. can v be a "toggle"? where press v once to go into visual mode and start moving, it should select those as well.
   Note the sidebar restrictions should still apply. should work wherever v works, please also add it to the file
   viewers, i want to use y to yank them to my clipboard. Remove any ]v stuff first since file viewer is getting and it
   would conflict with [ ] keybirds. Check for side effects
1. Interactive rebase, switch refresh to Ctrl-R, use shift r for interactive rebase
1. Pressing e on any "file" row should open $EDITOR, same as local changes, mainly changes and files
