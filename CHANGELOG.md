# Changelog

Each release on GitHub shows the section for its version.

## v0.3.5

### Fixed
- Renewing a player who is **On Hold** now takes him off hold, so he no longer leaves at the end of the
  season (#2). Up to v0.3.4 the contract changed, but the player stayed On Hold in Football Life 2027 saves.
  This works for saved files and for "write into the running game".
- Contract renewals no longer fail with "Couldn't read the in-game date from the save" when the game saved
  during a day and the save shows a time (for example `31/8/2027 14:00`, the last day of the transfer
  window).
