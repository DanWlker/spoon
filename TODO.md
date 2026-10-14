TODO:

1. should be able to render with `delta` check how lazygit does it
2. better designs, refer to how charmbracelet styles their TUI stuff
3. support multiple themes out of the box? catppuccin, tokyonight, rosepine
4. commands on the tree view: diff, cherry-pick, view only certain branch, these should use the `:` key similar to vim,
   then users can manually keybind them if they'd like to. these need to support picked hash as well
5. push pull fetch keybinds (these need to be commands as well, keybinds just invoke them), f may conflict with filter,
   need to think
6. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules
7. tabbing is working currently, just maybe need a better ui design cause the tab flashing always doesn't seem good
8. `]m` to move between marked selections
