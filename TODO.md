TODO:

1. Submodules should open a new instance, and they should be able to switch back, need to think about the ux there
   - Maybe at the bottom (aligned bottom) of the side tab is a projects section, where we can switch the active instance
     between the project, and its submodules
1. Move commits (do we really need this?)...
1. git blame view for a file, `B` will open a new window (similar to the help file i guess) showing git blame
   - on the right is the whole file, on the left is who changed, if the change span several lines only the first line should have the name of the blame
     - the left
       - space should ask whether you want to checkout,
       - y copy etc., same as below roughly
       - enter will ask whether you want to view that commit on the graph, it will then close the window back to the graph with it highlighted
       - o open the thing at that commit
     - right should be able to do whatever the file part was able to do
   - [ and ] should navigate between the latest commits that change the file, which would also change the blame at "that" commit
   - a top bar should show the current commit with < > to indicate it can move between commits
   - on the top bar commit thingy,
     - pressing y copies the commit hash,
     - space will ask whether you want to checkout, then close the window and check out
     - Enter will open something to let you search a certain commit / date, idk yet we see how
     - o open the thing at that commit (we probably should consider a commit object, file object stuff at this point to unify these functionalities & behaviours)
1. `T` timeline on a file, it should show all the commits related to this file reopened in a new window, at the right is the changes at each commit
   same actions apply, y copy, space checkout, enter view the commit on the graph, ] [ to navigate between the commits, o open the thing at that commit
