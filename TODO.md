TODO:

1. should be able to render with `delta` check how lazygit does it
1. support multiple themes out of the box? catppuccin, tokyonight, rosepine
1. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules
1. 3, 4 should not jump to the diff panel
1. Status bar should have similr design to my current neovim status bar design
1. Check whether can use tree-sitter rather than bat
1. when in diff mode, attempting to do more diffs should not be possible (shift d, selecting more and pressing shift d etc)
   - when in diff mode (from sidebar) pressing enter should show the info details for the selected commit, escape should go back to the diff, then another escape only exits the sidebar diff
1. master doesn't need a separate color
1. in the side bar, show how many commits missing from upstream and how many yet to push for the local ones
