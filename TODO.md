TODO:

1. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules
1. Move commits (do we really need this?)...
1. pressing a should work on Remotes on the sidebar to add new remote
1. when filling in the popup, and there is missing fields, ex. new remote start both is empty, pressing enter should move to the next empty one instead of showing error of course it should only work on required text fields
   - this also includes when the unstage part is empty and enter is pressed, it should jump to the commit window and try to commit (local changes)
1. Does tags conflict when there are same tags for multiple remotes, if yes how are they separated
1. this should work at the [3] changes at the info tab, pressing `u` can reset file, it should have a popup that ask you whether you want to reset file to state at commit or state before commit
1. for `o`, it should ask want to open which remote if there are multiple
1. also stuff like open in browser popup have alot of blank lines at the top, make it more focused and no so much blank lines
   - fix this for other popups as well (if exists)
1. remove the 0/1 for blame, we need a way to dynamically register keybinds and not manually, cause we've been trying to manually fix keybinds not showing up in the help
1. add more icons, make sure the Refs inside the Info have the same decoration, make a shared thing so that future stuff renders consistently
   1. tag should have an icon on the commit list
      󰓹 v1.2.0
   1. stash should have an icon
       stash
   1. remote branches should have an icon
      󰅟 master
1. In the blame, the panels should be able to increase width using ctrl left right and alt to move just like the others. also make this a shared thing? cause it already exists somewhere else but we keep missing it
