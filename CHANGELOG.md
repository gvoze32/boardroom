# Changelog

Each release on GitHub shows the section for its version.

## v0.3.6

### Added
- **My squad & contracts**: search by name or ID, a filter for players in the squad, loaned in or loaned out,
  and a Loan column (also in the CSV export). Players you loaned out are listed now (their contract is still
  yours); players you borrowed show the club they came from and when the loan ends.

### Fixed
- A transfer budget the game took below zero (compensation paid in the game) showed as about €429 million and
  locked budget editing. It is read as negative now; sales add to it, purchases wait until it is high enough.
- After a deal accepted in the game, the game also keeps your transfer budget in your club's record and can
  take it back from there. Budget changes now update that copy too, only when it holds the same amount.
- Agreed deals no longer stop with an error when the game's news list (200 rows) is full: like the game, the
  oldest routine news item at least 60 days old is reused.
- Writing into the running game finds the in-game date in GigaPack worlds, where it lies further in memory.

## v0.3.5

### Fixed
- Renewing a player who is **On Hold** now takes him off hold, so he no longer leaves at the end of the
  season (#2). Up to v0.3.4 the contract changed, but the player stayed On Hold in Football Life 2027 saves.
  This works for saved files and for "write into the running game".
- Contract renewals no longer fail with "Couldn't read the in-game date from the save" when the game saved
  during a day and the save shows a time (for example `31/8/2027 14:00`, the last day of the transfer
  window).
