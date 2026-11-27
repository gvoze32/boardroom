# Boardroom

A simple Windows app for editing your **Football Life 2027**, **Football Life
2026** or **PES 2021** Master League save while your career is in progress.

What you can do:

- **Sign players for your club.** You choose the transfer fee, the yearly
  salary and when the contract ends. The fee is taken from your transfer
  budget. The deal is agreed like in the game: the player shows in your
  negotiations and the game moves him to your squad on the next in-game day.
- **Sell players from your club.** Their contract ends and the fee goes into
  your transfer budget.
- **Loan players in or out.** You choose the loan fee and when the loan ends.
  The loan is agreed like in the game and the player moves on the next
  in-game day; his contract stays with the club that loans him out.
- **Move players between other (AI) clubs.**
- **Renew contracts:** give a player of your club a new yearly salary,
  contract end date and release clause, also when the game has asked you to
  renew him.
- **Release players:** end a contract without a new club, optionally paying a
  compensation from your transfer budget.
- **See your squad's contracts:** salaries, end dates, contracts ending within
  12 months and the total wage bill.
- **Filter players** by position, OVR and age, and **export** the player list
  or your squad to a CSV file (opens in Excel).
- **Change your finances:** transfer budget, yearly salary budget and manager
  name.
- **Turn a league match into a win.** After a league match is over (played
  or skipped in the game), pick it and set a winning score; the league table is
  recalculated.
- **Write changes into the running game**, without going back to the main
  menu and loading the save again: budget, manager name, contract renewals,
  and signings, sales and loans (the game itself completes those at the end
  of the day).

The app tells you when a new version is available.

## Download

Go to the [Releases](../../releases) page and download
`Boardroom-vX.Y.Z-windows-x64.exe`. Nothing to install: just open it.

## How to use

1. In the game, save your Master League and **go back to the main menu or close
   the game**.
2. Open `Boardroom-vX.Y.Z-windows-x64.exe`.
3. The app starts on **Get started**. Click **Open ML save…**, or drag your
   save file onto the window. Saves are in
   `Documents\KONAMI\eFootball PES 2021 SEASON UPDATE\<folder>\save\`
   (Football Life 2027: `Documents\SPGAME\Football Life\2027\save\`)
   and are named like `ML00000001`.
   - The app finds your club and the in-game date by itself; check
     **Your club** and change it if needed.
4. **Game folder…**: choose the folder where the game is installed. This fills
   in the names, positions, ages and OVR of all players.
5. The EDIT file (`EDIT00000000`, with players you edited in the game) is
   loaded by itself from your save folder; only if it isn't there does the
   app ask for it (**Open EDIT file…**). Its names win over the game folder.
   Then press **Continue →**.
   - The app remembers the save and the game folder: next time it loads them
     again and opens the tabs by itself. **Change save / setup…** above the
     tabs brings this screen back.
6. **Player transfers** tab: search for a player, fill in **Transfer fee**,
   **Salary per year** and **Contract until** (or click **+1 yr** to **+5 yr**
   from the in-game date) and, if you want one, a **Release clause** (empty =
   none), then press **Transfer →** below.
   - Picking a player to sign fills in normal terms for your club, medians
     of the 5 players in your squad closest to him in OVR: their
     **Salary per year**, their **Release clause** (of those who have one;
     none if nobody has) and, as the save has no market value, that clause
     as the **Transfer fee** too (0 for a player without a club). The
     contract runs 3 years (2 from age 30, 1 from age 33). The line under
     the form says which players they came from; change anything you like.
   - You can type amounts like `12,500,000`, `12.5m` or `750k`.
   - **Position**, **OVR** and **Age** narrow the list (players whose OVR or
     age isn't known are hidden while a filter is on). **Export list (CSV)…**
     saves the list as shown.
   - Tick **Loan** to loan the player in (from an AI club) or out (one of
     your players), fill in **Loan fee** and **Loan until** (**31 Dec** or
     **30 Jun**, the dates the game uses), then press **Loan →**. There is no
     salary to enter: the game stores none for a loan.
   - If you pick one of your own players, the app switches to selling.
   - A player you are already negotiating for in the game is refused: finish
     or cancel that negotiation in the game first.
   - **Wait for transfer window** (ticked by default): a signing or loan
     agreed while the window is closed (it is open 1 July – 31 August and
     1–31 January) waits in your negotiations and the player arrives the
     day the next window opens, as in the game. Untick it, or agree inside
     a window, and he arrives the next day.
   - A player without a club (free agent) arrives the same way; in the
     game he only negotiates his salary, so leave the fee at 0.
   - Saves the app can't read the transfer lists of: the player joins your
     squad right away instead; with the window closed, only if **Wait for
     transfer window** is unticked.
7. **My squad & contracts** tab: your players sorted by contract end; contracts
   ending within 12 months are highlighted. Select a player:
   - **Apply contract** sets the new **Salary per year**, **Contract until**
     (**+1 yr** to **+5 yr** from the in-game date) and **Release clause**
     (empty = none). A player who refused to renew no longer leaves when his
     contract ends. If the game has asked you to renew him (an open renewal
     request), the new contract answers that request like **Accept Terms**
     and can only go into the running game (see below).
   - **Release player** ends his contract and takes him out of your squad.
     **Remaining salary** fills in what is still owed until the contract ends;
     with **Pay from transfer budget** it is taken from the budget.
   - **Export squad (CSV)…** saves the squad with salaries and contract dates.
8. **Budget & manager** tab: the **Finances** box holds both budgets, the
   **Manager** box the manager name; each has its own **Apply** button. Both
   budgets are shown as in the game: the salary budget is what is left after
   your club's salaries, including signings agreed but not arrived yet.
9. **League results** tab: your club's league matches and the league table.
   Play or skip the match in the game first, save and go back to the
   main menu. Then select the played match, set **Your goals** higher than
   **Opponent's goals** and press **Make it a win**. The score and the table
   (points, wins/draws/losses, goals, positions) change.
10. Click **Save**. A copy of your old save is kept in the `Boardroom_backup`
    folder next to it, just in case.
11. Load the save in the game and check your squad, finances and league table.

You can undo changes one by one, cancel any single change with the **✖** next
to it, or **Cancel all changes**, before saving. A change that a later one
depends on (for example the move of a player you then moved again) can only
be cancelled after that later one.
**Currency** only changes how amounts are shown and typed; it uses the same
exchange rates as the game.

### Without leaving your Master League

Budget changes, the manager name, contract renewals, signings (also free
agents), sales, moves between other clubs and loans can go straight into the
game while your career is loaded there. This is beta: new and not tried in the
game yet. To save changes the usual way instead, use **Save** with the game on
the main menu.

1. Save in the game, then open that save in the app (if it was already open,
   open it again). Don't play a day in between.
2. Make the changes as usual.
3. Click **Write into running game (beta)** at the bottom. The game must be
   running with this Master League loaded.
4. Transfers and loans: go back to the game and advance one day. The
   game agrees them as if you had agreed them there and completes them at the
   end of that day (or when the transfer window opens, if they wait for it):
   the player moves, the fee and wages go through your budget, the news shows
   the deal. The app tells you when the game has done it.
5. Check the changes in the game, then save in the game as usual.

Changes written this way show **In game** in the list of changes and can't be
undone in the app; make a new change instead. Releases and league results
still need the usual way (**Save**, with the game on the main menu). A budget
change can't go in together with deals the game charges to your budget: write
it on its own. The app refuses and writes nothing when the game holds
different values than the opened save, for example after a day has passed, or
when the game's code isn't the one the app knows (other game versions, or
another tool such as The Gaffer's live deals already hooked into the same
place). It looks for `FL_2027.exe`, `FL_2026.exe`, `PES2021.exe` and the
programs in your game folder. If the game runs as administrator, start the app
as administrator too.

## Good to know

- **League results** haven't been tested in the game yet: after loading,
  check the result and the table. Only league matches of your club that have
  been played can be changed, and only into a win. Goal scorers, player
  stats, the manager's record and everything else the game did after the match
  stay as they were played (extra goals have no scorer). Cup matches can't be
  changed: the next round is already drawn with the real winner. A league
  table that doesn't add up to its results (for example after a points
  deduction) or that the app can't sort the way the game does is never
  changed. Matches can't be skipped: the game itself plays every match day.
- **Loans** haven't been tested in the game yet, and only up to the day the
  player moves: what the game does when a loan ends is unknown. Check the
  player and your budget in the game after loading. Players without a club,
  and loans between two AI clubs, can't be loaned.
- A player on loan at your club has no contract with you, so the
  **My squad & contracts** tab shows him as "no contract" and he can't be
  renewed, released or sold. A player you loaned out keeps his contract with
  you, so his salary still counts in the wages the app shows.
- **Free agents:** players listed under "Others (national team, no club)" may
  be free agents in the game. Moving free agents with an editor is known to
  cause "ghost players", so check them in the game first.
- **Selling, releasing and moving players between AI clubs** update the
  player's own data in the save the way the game does on a transfer: his
  previous club, join date, competition registrations, shirt number and your
  "My Team" list. **Renewing** also writes the new salary and end date into
  his data, which the game keeps next to the contract. The game registers
  moved players for competitions again; this has not been played through a
  season change yet, so check in the game after the next season starts.
- Selling or releasing a player from your starting eleven moves the next
  player in your "My Team" list up: check your line-up in the game.
- A sold or moved player keeps his shirt number when it is free at the new
  club, otherwise he gets the lowest free number. Players on loan can't be
  sold, released or moved by the app.
- If the app can't read the players' data in a save, selling, releasing,
  renewing and moving players between AI clubs are locked (a warning shows
  when the save opens); signings and loans still work.
- Contracts are only changed for your own club.
- A club can have at most 40 players. Line-ups are set in the game.
- The game's finance report won't list the fees paid through the app.
- On a PES 2021 save without the Football Life patch, budget editing has not
  been tested yet. If the app can't find the budget, it won't let you edit it.
- **Football Life 2027** saves have only been opened, edited, saved and opened
  again by the app; none has been loaded in the game after editing yet. Check
  your squad, budget and league table in the game. For FL27 player names, OVR
  and ages, choose the FL27 game folder (step 4).
- **FL Mod Studio worlds** (such as GigaPack by MatoZanato, about 150 MB
  saves) open like any other save: the app measures where Mod Studio's bigger
  tables have moved the budget, manager and transfer lists. Checked on two
  GigaPack saves (budget, salary budget and manager name match the game);
  none has been loaded in the game after editing yet. If a save's layout
  can't be measured, budget, manager and agreed deals are locked instead of
  guessed. The club list has all the world's clubs (5,080 in GigaPack).
- **Manager name**: up to v0.3.3 the app showed and changed the wrong coach
  (another club's). It now changes your own manager.
- **Writing into the running game** has not been tried in the game yet.
  Back up your save folder first and check the changes in the game before you
  save there. Transfers and loans put a small piece of code into the running
  game (at its end-of-day routine) that calls the game's own transfer
  routines; it only lives until the game is closed.
- **Release clauses** are stored in the contract. Up to v0.2.12 the app wrote
  the transfer fee there for new signings, which gave those players a release
  clause equal to their fee; renew them to change or remove it.

## Credits

Credits and third-party licences: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
