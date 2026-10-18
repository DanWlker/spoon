TODO:

1. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules
1. Move commits (do we really need this?)...
1. Changes & local changes technically are similar just without some keybinds, please ensure they share the same ui module, because the changes tab currently is not showing the root `/` folder
1. the enter to commit when unstaged is empty should also work for amend, rebase etc. they should share the same path
1. Check any functions or flows accepting strings and where types abstraction would be better. code smells would be like if or switches against constants
