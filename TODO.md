TODO:

1. should be able to render with `delta` check how lazygit does it
1. support multiple themes out of the box? catppuccin, tokyonight, rosepine
1. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules

Skipped (it would change behaviour or need a bigger refactor)

- Generate the sidebar hints from the commands themselves: the hints duplicate what each command accepts, but the commands call git, which is too slow to run every frame. Fixing this properly means a separate "does this command apply here" check per command.
- One focus type per view: the panel name, key scope and alt-arrow movement would all come from it. This is a refactor across views.
- Show the popup title as a badge like the other focused panels: it's a visual change, so it's your call.
- The remote lookup in the sidebar hints: it runs git, but it predates this work, and since the status bar stopped showing panel keys it only runs while the popup is open on a remote folder.

Simplifications if you're willing to drop a behaviour 2. Send sidebar b/t through the graph command path: removes about 25 lines and one of two routes to the same dialogs. Lost:

- On a header you'd get an error toast instead of nothing.
- On the cheaper variant, the dialog fills in a short hash instead of the ref name, and a branch made from a remote branch no longer tracks it.
