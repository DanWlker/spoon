TODO:

1. should be able to render with `delta` check how lazygit does it
1. support multiple themes out of the box? catppuccin, tokyonight, rosepine
1. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules
1. the text (HEAD) should not be shown in graph
   - with this, beautify the graph names on the graph to have like background colors
1. should dim commit descriptions that are not on the current branch
   - to better fit this change, the commit name, messages hash should have the same color
1. should be able to edit remote origin
1. should be able to click m on branches to merge into current branch, basically actions on branches should work on branches on the sidebar as well
1. `t` on commit graph should be able to create a tag
1. drop should work with multiple selection as well
1. `o` on commit graph should show the commits for the current active branch
