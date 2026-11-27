//! Master League save editing: club rosters, contracts and club finances.
//!
//! Layout of the decrypted `data` block (PES 2021 1.04+ and Football Life,
//! both run the same Master League code):
//!
//! * Club table at 0x50, 0x690 bytes per club: name +0x04, abbreviation +0x4A,
//!   roster of 40 `(internal_id, player_id)` pairs at +0x14C (empty =
//!   `(0xFFFF, 0)`), roster count in the low 7 bits of the u16 at +0x426,
//!   shirt numbers (u16, same order as the roster, 0 after the last player)
//!   at +0x29A.
//!   Clubs come first, then national teams (Albania/ALB first) up to The Old Boys.
//! * Finance block: transfer budget u32, three zero words, a word that is 0
//!   in the first season and later holds the transfer budget the season
//!   started with (left alone), then the annual salary budget u32 (+0x14),
//!   both budgets in units of 100 EUR. Football Life 2026
//!   keeps it at 0xC7DD00 (confirmed against the in-game value), FL27 too
//!   (found there in a real save, value not checked in game); PES 2021 at
//!   0xC7DE00 (ML Money Tool by Devil Cold52). The game shows the salary budget
//!   minus the club's wage bill, and charges the transfer fee when a deal is
//!   agreed. Read and edited separately from the manager name, so one being
//!   unreadable doesn't lock the other.
//! * Coaches at 0x133A30, 0x258 bytes each: u32 index, u32 coach id, name
//!   (UTF-8, 46 bytes). A club record holds its manager's handle (index, id) at
//!   +0x28C. The user's manager is the coach of the club "My Team" refers to;
//!   the game keeps the same id and name in two more places (a second coach
//!   record and a copy in the career state), and a rename writes all of them.
//! * "My Team" list of the managed club at 0xB7F8BC: 16-byte rows
//!   `(internal id, player id, 0, flag)`, flag 0xC0 in the squad, 0xC1 just
//!   arrived, 0xC2 on loan, 0xC7 free; club reference at 0xB7F804.
//! * Player records: `playerdb`. Moves the editor makes itself (sales, moves
//!   between other clubs, releases, signings that join right away) and
//!   renewals update them like the game does.
//! * The save description holds the managed club, its league and the in-game
//!   date (`Chelsea FC / Premier League\n6/8/2025\n…`).
//! * League matches and tables: `league`.
//!
//! The offsets are the plain game's. An FL Mod Studio world (GigaPack: 5,080
//! clubs, 8,000 club slots) grows the tables and moves the ones after them;
//! `Layout` measures where each save keeps them.
//!
//! Every edit goes through `patch` (data block) or `patch_record` (player
//! records), which journal the previous bytes, so undo is byte-exact and
//! saving can prove that nothing outside the journal changed.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::contracts::{Contract, ContractTable, Date, SALARY_UNIT, STATUS_AGREED};
use crate::deals::{self, AgreedDeal, DealTables, LoanDeal};
use crate::league::{Fixture, League, Standing};
use crate::playerdb::{self, Move, PlayerDb};
use crate::{Error, crypto, live};

pub const TABLE_START: usize = 0x50;
pub const RECORD_SIZE: usize = 0x690;
pub const NAME_OFFSET: usize = 0x04;
pub const NAME_SIZE: usize = 0x46;
pub const ABBR_OFFSET: usize = 0x4A;
pub const ABBR_SIZE: usize = 6;
pub const ROSTER_OFFSET: usize = 0x14C;
pub const COUNT_OFFSET: usize = 0x426;
pub const COUNT_MASK: u16 = 0x7F;
pub const NUMBERS_OFFSET: usize = 0x29A;
pub const MAX_ROSTER: usize = 40;
pub const EMPTY: Player = Player { internal_id: 0xFFFF, player_id: 0 };
/// FL Mod Studio worlds (GigaPack: 5,239 clubs) write saves of about 150 MB.
const MAX_SAVE: u64 = 400_000_000;

pub const FL26_FINANCE: usize = 0xC7DD00;
pub const PES21_FINANCE: usize = 0xC7DE00;
pub const SALARY_BUDGET_OFFSET: usize = 0x14;
/// Transfer budget at the start of the season; 0 during the first season.
const SEASON_START_BUDGET_OFFSET: usize = 0x10;
pub const COACH_TABLE: usize = 0x13_3A30;
pub const COACH_RECORD: usize = 0x258;
/// Coach record: u32 index, u32 coach id, then the name.
pub const COACH_NAME: usize = 8;
pub const MANAGER_NAME_SIZE: usize = 46;
/// Club record: handle (u32 coach index, u32 coach id) of the club's manager.
pub const TEAM_COACH: usize = 0x28C;
/// Lowest transfer budget a save is believed to hold (−500 million EUR).
const MIN_TRANSFER_UNITS: i32 = -5_000_000;
/// Club record: `0xFFFF_FFF5` here marks the club the user manages.
const CLUB_USER_MARK: usize = 0x290;
const USER_CLUB: u32 = 0xFFFF_FFF5;
/// Club record: i32 transfer budget (units of 100 EUR), see `set_transfer_units`.
const CLUB_BUDGET: usize = 0x644;
pub const MY_TEAM_CLUB: usize = 0xB7_F804;
pub const MY_TEAM: usize = 0xB7_F8BC;
pub const MY_TEAM_ROW: usize = 16;
const MY_TEAM_ROWS: usize = 64;
const MY_TEAM_FREE: u32 = 0xC7;
pub const MONEY_UNIT: i64 = 100;
/// Bytes at the end of the data block searched for a player's renewal state row.
const RENEWAL_STATE_WINDOW: usize = 0x10000;
/// Bytes of a renewal state row's last word that are always zero; the high
/// byte varies per player.
pub(crate) const STATE_ROW_ZERO_MASK: u32 = 0x00FF_FFFF;

pub const NATIONAL_FIRST: [&str; 4] = ["ALB", "AND", "ARM", "AUT"];
const NATIONAL_FIRST_NAMES: [&str; 4] = ["albania", "andorra", "armenia", "austria"];
/// Backups of overwritten saves, next to the save.
pub const BACKUP_DIR: &str = "Boardroom_backup";

/// Club table capacity of the plain game.
pub const PLAIN_TEAMS: usize = 750;
/// Club references keep the club index in 14 bits.
const MAX_TEAMS: usize = 0x4000;
/// Per-club side array between "My Team" and the finance block: 16 bytes per club slot.
const TEAM_SIDE_ROW: usize = 0x10;

/// Where the tables the editor finds at fixed offsets lie. FL Mod Studio
/// (GigaPack and other worlds) raises the game's caps, which grows arrays in
/// the data block and moves everything after them; the arrays and their order
/// are those of Mod Studio's patch sets (`mlcopy.regions`), confirmed on two
/// GigaPack saves. Measured per save, not taken from a list of sets:
/// * the club table's capacity (`team_cap`) moves the coach table behind it;
/// * `middle_shift` moves what lies between the calendar and the per-club side
///   array ("My Team", news): the growth of clubs, coaches, regulations,
///   fixtures, matches and calendar;
/// * the side array (16 bytes per club slot) adds to that for the finance
///   block, season deals and pending arrivals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub team_cap: usize,
    pub middle_shift: usize,
}

impl Layout {
    pub const PLAIN: Layout = Layout { team_cap: PLAIN_TEAMS, middle_shift: 0 };

    pub fn coaches(&self) -> usize {
        COACH_TABLE + (self.team_cap - PLAIN_TEAMS) * RECORD_SIZE
    }

    /// A table between the calendar and the side array, at `plain` in the plain game.
    pub fn middle(&self, plain: usize) -> usize {
        plain + self.middle_shift
    }

    /// A table after the side array, at `plain` in the plain game.
    pub fn upper(&self, plain: usize) -> usize {
        plain + self.middle_shift + (self.team_cap - PLAIN_TEAMS) * TEAM_SIDE_ROW
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Team {
    pub index: usize,
    pub name: String,
    pub abbr: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Player {
    pub internal_id: u32,
    pub player_id: u32,
}

/// Where a transferred player comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Club(usize),
    /// National-team players without a club; some may be free agents.
    Others,
}

/// Transfer and salary budget; the manager name is separate (`manager_name`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finances {
    pub transfer_eur: i64,
    /// Annual salary budget; the game shows this minus the wage bill.
    pub salary_eur: i64,
}

/// The managed club's league as the "League results" tab shows it.
#[derive(Clone, Debug)]
pub struct LeagueView {
    /// From the save description, or "League".
    pub name: String,
    /// The managed club's reference in `fixtures` and `standings`.
    pub club: u32,
    pub fixtures: Vec<Fixture>,
    pub standings: Vec<Standing>,
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub description: String,
    /// The request that made it, to do it again after cancelling an earlier change.
    redo: Redo,
    /// (offset, previous bytes) in write order.
    patches: Vec<(usize, Vec<u8>)>,
    /// Same for the player records.
    record_patches: Vec<(usize, Vec<u8>)>,
    /// Answers a request the game has open (a renewal request): only the
    /// running game can take it (`live`), not the file.
    live_only: bool,
}

#[derive(Clone, Debug)]
enum Redo {
    Transfer(TransferRequest),
    Loan(LoanRequest),
    Renew { player: Player, salary_eur: i64, end: Date, release_eur: Option<i64>, name: Option<String> },
    Release { player: Player, compensation_eur: i64, use_transfer_budget: bool, name: Option<String> },
    Finances { transfer_eur: i64, salary_eur: i64 },
    Manager(String),
    LeagueWin { index: usize, our_goals: u8, their_goals: u8 },
}

#[derive(Clone, Debug)]
pub struct TransferRequest {
    pub source: Source,
    pub dest: usize,
    pub player_id: u32,
    pub fee_eur: i64,
    pub salary_eur: Option<i64>,
    pub contract_end: Option<Date>,
    /// Release clause of an incoming player's new contract; `None` = no clause.
    pub release_clause_eur: Option<i64>,
    /// Charge (incoming) or credit (outgoing) the fee to the transfer budget.
    pub use_transfer_budget: bool,
    /// An incoming deal waits for the next transfer window when it is closed;
    /// otherwise the player arrives the next day.
    pub wait_for_window: bool,
    pub name: Option<String>,
}

impl TransferRequest {
    pub fn new(source: Source, dest: usize, player_id: u32) -> Self {
        TransferRequest {
            source,
            dest,
            player_id,
            fee_eur: 0,
            salary_eur: None,
            contract_end: None,
            release_clause_eur: None,
            use_transfer_budget: true,
            wait_for_window: true,
            name: None,
        }
    }
}

/// A player's running loan, as his record holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoanInfo {
    /// Loan end; `None` when the record holds no date.
    pub until: Option<Date>,
    /// Club he joined from this season (for a loan in, the parent club).
    pub from: Option<usize>,
}

/// A loan to or from the managed club.
#[derive(Clone, Debug)]
pub struct LoanRequest {
    /// The other club: the one the player comes from (loan in) or goes to (loan out).
    pub club: usize,
    /// True when the managed club loans its own player out.
    pub outgoing: bool,
    pub player_id: u32,
    pub fee_eur: i64,
    pub loan_end: Date,
    /// Charge (loan in) or credit (loan out) the fee to the transfer budget.
    pub use_transfer_budget: bool,
    /// The loan waits for the next transfer window when it is closed;
    /// otherwise the player moves the next day.
    pub wait_for_window: bool,
    pub name: Option<String>,
}

/// `round(value / divisor)` with ties to even, like Python's `round`.
pub fn div_round_even(value: i64, divisor: i64) -> i64 {
    let q = value.div_euclid(divisor);
    let r = value.rem_euclid(divisor) * 2;
    if r > divisor || (r == divisor && q % 2 != 0) { q + 1 } else { q }
}

pub fn eur_to_units(eur: i64) -> i64 {
    div_round_even(eur, MONEY_UNIT)
}

/// A release clause in the contract's units (€100); 0 = no clause.
fn release_units(eur: i64) -> Result<u32, Error> {
    u32::try_from(eur_to_units(eur))
        .ok()
        .filter(|u| *u < 0x8000_0000)
        .ok_or_else(|| Error::new("The release clause is too high or negative."))
}

pub fn fmt_int(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0 { format!("-{out}") } else { out }
}

pub fn fmt_eur(eur: i64) -> String {
    format!("€{}", fmt_int(eur))
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

/// A NUL-padded field holding one line of UTF-8 text.
fn name_field_ok(field: &[u8]) -> bool {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).is_ok_and(|n| !n.trim().is_empty() && !n.chars().any(|c| c < ' '))
        && field[end..].iter().all(|&b| b == 0)
}

fn text_at(data: &[u8], off: usize, size: usize) -> String {
    let raw = &data[off..off + size];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).trim().to_string()
}

pub struct MasterLeague {
    pub path: PathBuf,
    save: crypto::SaveFile,
    pub game: String,
    pub data: Vec<u8>,
    pub history: Vec<Operation>,
    current: Option<Operation>,
    pub managed_team: Option<usize>,
    pub contracts: Option<ContractTable>,
    pub teams: Vec<Team>,
    national: Range<usize>,
    /// Where the fixed-offset tables lie; `None` when unknown (budget, manager,
    /// "My Team" and the deal lists are then left alone).
    pub layout: Option<Layout>,
    finance_offset: Option<usize>,
    pub finance_error: Option<String>,
    /// Why the manager name can't be edited; `None` when it can.
    pub manager_error: Option<String>,
    /// Every copy of the manager's name field, the coach record first.
    manager_names: Vec<usize>,
    /// In-game date from the save description.
    pub game_date: Option<Date>,
    /// Managed club named in the save description.
    pub described_club: Option<usize>,
    /// That club's league, from the description.
    pub league_name: Option<String>,
    /// Player records as opened; `records` holds the edited copy.
    players: Option<PlayerDb>,
    pub records: Vec<u8>,
    /// Why the player records can't be read; selling, releasing, renewing and
    /// moving players between other clubs need them.
    pub players_error: Option<String>,
    /// The first `live_written` changes in `history` are already in the
    /// running game (`live`); they can't be undone or cancelled here.
    live_written: usize,
}

impl MasterLeague {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path).map_err(|_| Error::new("File not found."))?;
        let meta = std::fs::metadata(&path).map_err(|_| Error::new("File not found."))?;
        if !meta.is_file() || meta.len() > MAX_SAVE {
            return Err(Error::new("File not found, or its size doesn't look like an ML save."));
        }
        let blob = std::fs::read(&path).map_err(|e| Error::new(e.to_string()))?;
        let save = crypto::unpack(&blob, true)?;
        if save.file_type() != "ML" {
            let t = save.file_type();
            return Err(Error::new(format!(
                "This isn't a Master League save (file type: {}).",
                if t.is_empty() { "?" } else { &t }
            )));
        }
        let mut game = MasterLeague {
            path,
            game: save.game_version(),
            data: save.data.clone(),
            save,
            history: Vec::new(),
            current: None,
            managed_team: None,
            contracts: None,
            teams: Vec::new(),
            national: 0..0,
            layout: None,
            finance_offset: None,
            finance_error: None,
            manager_error: None,
            manager_names: Vec::new(),
            game_date: None,
            described_club: None,
            league_name: None,
            players: None,
            records: Vec::new(),
            players_error: None,
            live_written: 0,
        };
        game.teams = game.discover_clubs()?;
        game.national = game.discover_national_teams()?;
        for i in 0..game.teams.len() {
            game.raw_roster(i)?;
        }
        game.load_players();
        game.layout = game.discover_layout();
        match game.locate_finances() {
            Ok(offset) => game.finance_offset = Some(offset),
            Err(e) => game.finance_error = Some(e),
        }
        match game.locate_manager() {
            Ok(offsets) => game.manager_names = offsets,
            Err(e) => game.manager_error = Some(e),
        }
        game.read_description();
        Ok(game)
    }

    fn load_players(&mut self) {
        match PlayerDb::locate(&self.data) {
            Ok(db) => {
                self.records = db.records.clone();
                self.players = Some(db);
                self.players_error = None;
            }
            Err(e) => {
                self.records.clear();
                self.players = None;
                self.players_error = Some(format!(
                    "{e} Selling, releasing, renewing and moving players between other clubs are locked."
                ));
            }
        }
    }

    /// Football Life 2027 save (encrypted with its own master key).
    pub fn is_fl27(&self) -> bool {
        self.save.master_key == &crypto::FL27_KEY
    }

    /// `"Chelsea FC / Premier League\n6/8/2025\nPremier League"` at desc+0x80;
    /// a German game writes the date as `12.9.2026`; a save made during a
    /// day adds the time (`31/8/2027 14:00`).
    fn read_description(&mut self) {
        let raw = self.save.desc.get(0x80..).unwrap_or(&[]);
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let text = String::from_utf8_lossy(&raw[..end]).into_owned();
        let mut lines = text.lines();
        let mut first = lines.next().map(|l| l.split(" / "));
        if let Some(club) = first.as_mut().and_then(|parts| parts.next()) {
            let matches: Vec<usize> = self.teams.iter().filter(|t| t.name == club.trim()).map(|t| t.index).collect();
            self.described_club = (matches.len() == 1).then(|| matches[0]);
        }
        self.league_name = first.and_then(|mut parts| parts.next()).map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
        self.game_date = lines.next().and_then(|line| {
            let date = line.split_whitespace().next()?;
            let mut parts = date.split(['/', '.']).map(|p| p.parse::<u16>().ok());
            let (d, m, y) = (parts.next()??, parts.next()??, parts.next()??);
            Date::new(y, u8::try_from(m).ok()?, u8::try_from(d).ok()?)
        });
    }

    // --- discovery -----------------------------------------------------
    fn base(index: usize) -> usize {
        TABLE_START + index * RECORD_SIZE
    }

    fn table_len(&self) -> usize {
        self.data.len().saturating_sub(TABLE_START) / RECORD_SIZE
    }

    fn team(&self, index: usize) -> Result<Team, Error> {
        let base = Self::base(index);
        if base + RECORD_SIZE > self.data.len() {
            return Err(Error::new("The club list in this save looks damaged."));
        }
        Ok(Team {
            index,
            name: text_at(&self.data, base + NAME_OFFSET, NAME_SIZE),
            abbr: text_at(&self.data, base + ABBR_OFFSET, ABBR_SIZE),
        })
    }

    fn is_national_start(&self, i: usize) -> Result<bool, Error> {
        let group = (i..i + 4).map(|j| self.team(j)).collect::<Result<Vec<_>, _>>()?;
        Ok(group.iter().zip(NATIONAL_FIRST).all(|(t, a)| t.abbr.to_uppercase() == a)
            || group.iter().zip(NATIONAL_FIRST_NAMES).all(|(t, n)| t.name.to_lowercase() == n))
    }

    fn discover_clubs(&self) -> Result<Vec<Team>, Error> {
        let limit = MAX_TEAMS.min(self.table_len().saturating_sub(4));
        let mut count = 0;
        for i in 0..limit {
            if self.is_national_start(i)? {
                count = i;
                break;
            }
        }
        if count < 400 {
            return Err(Error::new("Couldn't tell clubs and national teams apart; this save was not changed."));
        }
        let teams = (0..count).map(|i| self.team(i)).collect::<Result<Vec<_>, _>>()?;
        if teams.iter().any(|t| t.name.is_empty()) {
            return Err(Error::new("A club in this save has no name."));
        }
        Ok(teams)
    }

    fn discover_national_teams(&self) -> Result<Range<usize>, Error> {
        let first = self.teams.len();
        for i in first..(first + 200).min(self.table_len()) {
            let t = self.team(i)?;
            if t.abbr.to_uppercase() == "TOB" || t.name.to_lowercase() == "the old boys" {
                if (80..=180).contains(&(i - first)) {
                    return Ok(first..i);
                }
                break;
            }
        }
        Ok(0..0)
    }

    /// Where the tables the editor finds at fixed offsets lie in this save;
    /// `None` when that can't be told. PES 2021 has no Mod Studio: plain layout.
    fn discover_layout(&self) -> Option<Layout> {
        if !self.game.contains("Football Life") {
            return Some(Layout::PLAIN);
        }
        // The coach table follows the club table, so its start gives the club table's capacity.
        let team_cap = (PLAIN_TEAMS..=MAX_TEAMS).find(|&cap| {
            let coaches = Layout { team_cap: cap, middle_shift: 0 }.coaches();
            (0..4).all(|j| {
                let o = coaches + j * COACH_RECORD;
                o + COACH_RECORD <= self.data.len()
                    && u32_at(&self.data, o) == j as u32
                    && name_field_ok(&self.data[o + COACH_NAME..o + COACH_NAME + MANAGER_NAME_SIZE])
            })
        })?;
        // "My Team" gives the middle shift; it lies before the player records.
        let end = self.players.as_ref().map_or(self.data.len(), PlayerDb::start);
        let mut found = None;
        for at in (MY_TEAM_CLUB..end.saturating_sub(MY_TEAM - MY_TEAM_CLUB + MY_TEAM_ROWS * MY_TEAM_ROW)).step_by(4) {
            if self.is_my_team(at) && found.replace(at - MY_TEAM_CLUB).is_some() {
                return None;
            }
        }
        match found {
            Some(middle_shift) => Some(Layout { team_cap, middle_shift }),
            // No list to measure by: only a save without grown arrays is known.
            None => (team_cap == PLAIN_TEAMS).then_some(Layout::PLAIN),
        }
    }

    /// A "My Team" list at `at`: a club's reference, then 64 rows
    /// `(internal id, player id, 0, 0xC0…0xC7)` holding some of that club's squad.
    fn is_my_team(&self, at: usize) -> bool {
        let club_ref = u32_at(&self.data, at);
        let club = (club_ref & 0x3FFF) as usize;
        if club >= self.teams.len() || self.club_ref(club) != club_ref {
            return false;
        }
        let rows: Vec<[u32; 4]> = (0..MY_TEAM_ROWS)
            .map(|k| {
                let o = at + (MY_TEAM - MY_TEAM_CLUB) + k * MY_TEAM_ROW;
                core::array::from_fn(|w| u32_at(&self.data, o + 4 * w))
            })
            .collect();
        if rows.iter().any(|r| r[2] != 0 || !(0xC0..=MY_TEAM_FREE).contains(&r[3])) {
            return false;
        }
        let squad = self.club_roster(club);
        rows.iter().any(|r| r[3] != MY_TEAM_FREE && squad.contains(&Player { internal_id: r[0], player_id: r[1] }))
    }

    /// Finance block offset for this game, verified structurally; error text when untrusted.
    fn locate_finances(&self) -> Result<usize, String> {
        let not_found = || "Couldn't find the budget in this save, so it can't be edited.".to_string();
        // No cross-variant fallback: the other game's offset can hold a look-alike block.
        let plain = if self.game.contains("Football Life") { FL26_FINANCE } else { PES21_FINANCE };
        let off = self.layout.ok_or_else(not_found)?.upper(plain);
        let plausible = self.data.len() >= off + SALARY_BUDGET_OFFSET + 4
            && (4..SEASON_START_BUDGET_OFFSET).step_by(4).all(|gap| u32_at(&self.data, off + gap) == 0)
            // Signed: the game can take the transfer budget a little below zero.
            && (MIN_TRANSFER_UNITS..).contains(&(u32_at(&self.data, off) as i32))
            && (1..0x8000_0000).contains(&u32_at(&self.data, off + SALARY_BUDGET_OFFSET));
        if plausible { Ok(off) } else { Err(not_found()) }
    }

    /// The user's manager: the coach whose handle the "My Team" club's record
    /// holds, plus every other copy of that coach's id followed by the same
    /// name field. Offsets of the name fields, the coach record's first; error text otherwise.
    fn locate_manager(&self) -> Result<Vec<usize>, String> {
        let not_found = || "Couldn't find the manager name in this save, so it can't be edited.".to_string();
        let layout = self.layout.ok_or_else(not_found)?;
        let d = &self.data;
        let my_team_club = layout.middle(MY_TEAM_CLUB);
        if d.len() < my_team_club + 4 {
            return Err(not_found());
        }
        let club_ref = u32_at(d, my_team_club);
        let club = (club_ref & 0x3FFF) as usize;
        if club >= self.teams.len() || self.club_ref(club) != club_ref {
            return Err(not_found());
        }
        let handle = &d[Self::base(club) + TEAM_COACH..][..8];
        let record = (u32_at(handle, 0) as usize)
            .checked_mul(COACH_RECORD)
            .and_then(|o| o.checked_add(layout.coaches()))
            .filter(|&o| o + COACH_RECORD <= d.len() && d[o..o + 8] == *handle)
            .ok_or_else(not_found)?;
        let field = &d[record + COACH_NAME..][..MANAGER_NAME_SIZE];
        if !name_field_ok(field) {
            return Err(not_found());
        }
        let pattern = [&handle[4..], field].concat();
        let mut offsets = vec![record + COACH_NAME];
        offsets.extend(memchr::memmem::find_iter(d, &pattern).map(|o| o + 4).filter(|&o| o != record + COACH_NAME));
        Ok(offsets)
    }

    // --- rosters ---------------------------------------------------------
    fn raw_roster(&self, index: usize) -> Result<(usize, Vec<Player>), Error> {
        if index >= self.teams.len() && !self.national.contains(&index) {
            return Err(Error::new("Invalid club."));
        }
        let base = Self::base(index);
        let n = (u16_at(&self.data, base + COUNT_OFFSET) & COUNT_MASK) as usize;
        let name = self.team(index)?.name;
        if n > MAX_ROSTER {
            return Err(Error::new(format!("{name} has an invalid number of players: {n}.")));
        }
        let slots: Vec<Player> = (0..MAX_ROSTER)
            .map(|j| {
                let o = base + ROSTER_OFFSET + 8 * j;
                Player { internal_id: u32_at(&self.data, o), player_id: u32_at(&self.data, o + 4) }
            })
            .collect();
        if slots[..n].contains(&EMPTY) {
            return Err(Error::new(format!("{name}'s squad has an empty slot in the middle.")));
        }
        if slots[n..].iter().any(|p| *p != EMPTY) {
            return Err(Error::new(format!("{name}'s squad has unknown data in its empty slots.")));
        }
        Ok((n, slots))
    }

    fn club_roster(&self, index: usize) -> Vec<Player> {
        // Validated at open and after every write, so this cannot fail for known teams.
        let (n, mut slots) = self.raw_roster(index).expect("validated roster");
        slots.truncate(n);
        slots
    }

    pub fn roster(&self, source: Source) -> Vec<Player> {
        match source {
            Source::Club(index) => self.club_roster(index),
            Source::Others => self.other_candidates(),
        }
    }

    /// National-team players without a club. Some may be free agents.
    pub fn other_candidates(&self) -> Vec<Player> {
        let in_clubs: HashSet<u32> =
            (0..self.teams.len()).flat_map(|i| self.club_roster(i)).map(|p| p.player_id).collect();
        let mut found: BTreeMap<u32, Player> = BTreeMap::new();
        let mut ambiguous = HashSet::new();
        for i in self.national.clone() {
            for p in self.club_roster(i) {
                if p.player_id == 0 || p.internal_id == 0xFFFF || in_clubs.contains(&p.player_id) {
                    continue;
                }
                match found.get(&p.player_id) {
                    Some(prev) if *prev != p => {
                        ambiguous.insert(p.player_id);
                    }
                    Some(_) => {}
                    None => {
                        found.insert(p.player_id, p);
                    }
                }
            }
        }
        found.into_iter().filter(|(pid, _)| !ambiguous.contains(pid)).map(|(_, p)| p).collect()
    }

    pub fn club_of(&self, player_id: u32) -> Vec<usize> {
        (0..self.teams.len()).filter(|&i| self.club_roster(i).iter().any(|p| p.player_id == player_id)).collect()
    }

    pub fn source_name(&self, source: Source) -> String {
        match source {
            Source::Club(i) => self.teams[i].name.clone(),
            Source::Others => "Others (national team)".into(),
        }
    }

    // --- journaled writes -------------------------------------------------
    fn patch(&mut self, offset: usize, new: &[u8]) {
        let op = self.current.as_mut().expect("write outside an operation");
        op.patches.push((offset, self.data[offset..offset + new.len()].to_vec()));
        self.data[offset..offset + new.len()].copy_from_slice(new);
    }

    fn patch_record(&mut self, offset: usize, new: &[u8]) {
        let op = self.current.as_mut().expect("write outside an operation");
        op.record_patches.push((offset, self.records[offset..offset + new.len()].to_vec()));
        self.records[offset..offset + new.len()].copy_from_slice(new);
    }

    fn revert(&mut self, op: &Operation) {
        for (offset, old) in op.patches.iter().rev() {
            self.data[*offset..offset + old.len()].copy_from_slice(old);
        }
        for (offset, old) in op.record_patches.iter().rev() {
            self.records[*offset..offset + old.len()].copy_from_slice(old);
        }
    }

    fn begin(&mut self, redo: Redo) {
        self.current = Some(Operation {
            description: String::new(),
            redo,
            patches: Vec::new(),
            record_patches: Vec::new(),
            live_only: false,
        });
    }

    fn commit(&mut self, description: String) -> &Operation {
        let mut op = self.current.take().expect("no operation");
        op.description = description;
        self.history.push(op);
        self.history.last().unwrap()
    }

    fn rollback(&mut self) {
        if let Some(op) = self.current.take() {
            self.revert(&op);
        }
    }

    /// Run `body` as one journaled operation; any error restores the previous bytes.
    fn atomic<F>(&mut self, redo: Redo, body: F) -> Result<&Operation, Error>
    where
        F: FnOnce(&mut Self) -> Result<String, Error>,
    {
        self.begin(redo);
        match body(self) {
            Ok(description) => Ok(self.commit(description)),
            Err(e) => {
                self.rollback();
                Err(e)
            }
        }
    }

    pub fn undo(&mut self) -> Result<String, Error> {
        if self.history.len() <= self.live_written && !self.history.is_empty() {
            return Err(Error::new(Self::IN_GAME));
        }
        let op = self.history.pop().ok_or_else(|| Error::new("Nothing to undo."))?;
        self.revert(&op);
        Ok(op.description)
    }

    const IN_GAME: &str = "That change is already in the running game and can't be undone here. Make a new change \
                           instead (for example set the old budget again).";

    /// Cancel the `index`-th unsaved change and keep the later ones: undo back
    /// to it, then do the later ones again on top. If one of them no longer
    /// works without it (for example a sale of the player it signed), nothing
    /// changes.
    pub fn cancel(&mut self, index: usize) -> Result<String, Error> {
        if index >= self.history.len() {
            return Err(Error::new("That change no longer exists."));
        }
        if index < self.live_written {
            return Err(Error::new(Self::IN_GAME));
        }
        let before = (self.data.clone(), self.records.clone(), self.history.clone());
        let later = self.history.split_off(index + 1);
        let target = self.history.pop().unwrap();
        for op in later.iter().rev() {
            self.revert(op);
        }
        self.revert(&target);
        for op in &later {
            if let Err(e) = self.replay(&op.redo) {
                (self.data, self.records, self.history) = before;
                return Err(Error::new(format!(
                    "\"{}\" can't be cancelled on its own: the later change \"{}\" depends on it ({e}). Cancel that one first.",
                    target.description, op.description
                )));
            }
        }
        Ok(target.description)
    }

    /// Do a change again from its request. A contract, budget or manager name
    /// that is already as requested needs no change.
    fn replay(&mut self, redo: &Redo) -> Result<(), Error> {
        match redo.clone() {
            Redo::Transfer(req) => self.transfer(req).map(|_| ()),
            Redo::Loan(req) => self.loan(req).map(|_| ()),
            Redo::Renew { player, salary_eur, end, release_eur, name } => {
                let units = u32::try_from(div_round_even(salary_eur, SALARY_UNIT as i64)).ok();
                let release = release_eur.map(|e| u32::try_from(div_round_even(e, SALARY_UNIT as i64)).ok());
                if self.contract_of(&player).is_some_and(|c| {
                    Some(c.salary_units) == units && c.end == end && release.is_none_or(|r| r == Some(c.release_units))
                }) {
                    return Ok(());
                }
                self.renew_contract(player, salary_eur, end, release_eur, name.as_deref()).map(|_| ())
            }
            Redo::Release { player, compensation_eur, use_transfer_budget, name } => {
                self.release(player, compensation_eur, use_transfer_budget, name.as_deref()).map(|_| ())
            }
            Redo::Finances { transfer_eur, salary_eur } => {
                let now = self.finances()?;
                if (now.transfer_eur, now.salary_eur) == (transfer_eur, salary_eur) {
                    return Ok(());
                }
                self.set_finances(transfer_eur, salary_eur).map(|_| ())
            }
            Redo::Manager(name) => {
                if self.manager_name()? == name {
                    return Ok(());
                }
                self.set_manager_name(&name).map(|_| ())
            }
            Redo::LeagueWin { index, our_goals, their_goals } => self.set_league_win(index, our_goals, their_goals).map(|_| ()),
        }
    }

    pub fn operations(&self) -> Vec<&str> {
        self.history.iter().map(|o| o.description.as_str()).collect()
    }

    /// How many of the changes in `operations()` are already in the running game.
    pub fn written_to_game(&self) -> usize {
        self.live_written
    }

    /// What the changes not yet in the running game would write there, and
    /// the number of changes it covers (for `mark_written_to_game`). Budget,
    /// manager name and contract renewals go in as bytes; transfers and loans
    /// (beta) as deals the game agrees and completes itself.
    pub fn live_plan(&self) -> Result<(live::Plan, usize), Error> {
        let pending = &self.history[self.live_written..];
        if pending.is_empty() {
            return Err(Error::new("There are no new changes to write into the game."));
        }
        let is_deal = |r: &Redo| matches!(r, Redo::Transfer(_) | Redo::Loan(_));
        let supported = |r: &Redo| matches!(r, Redo::Finances { .. } | Redo::Manager(_) | Redo::Renew { .. }) || is_deal(r);
        if let Some(op) = pending.iter().find(|op| !supported(&op.redo)) {
            return Err(Error::new(format!(
                "\"{}\" can't go into the running game: releases and league results can't. Cancel it, or save it \
                 the usual way (game on the main menu).",
                op.description
            )));
        }
        // The save as the game has it: before the pending changes.
        let mut before = self.data.clone();
        for op in pending.iter().rev() {
            for (offset, old) in op.patches.iter().rev() {
                before[*offset..offset + old.len()].copy_from_slice(old);
            }
        }
        // The running game keeps (most of) the data before the player records byte for byte.
        let flat_end = self.players.as_ref().map_or(playerdb::MIN_START, PlayerDb::start);
        let mut changed = Vec::new();
        let mut renewed: Vec<(Player, Option<String>)> = Vec::new();
        let mut deals = Vec::new();
        for op in pending {
            match &op.redo {
                Redo::Renew { player, name, .. } => {
                    renewed.retain(|(p, _)| p != player);
                    renewed.push((*player, name.clone()));
                }
                // The game does the whole move itself, money included; the
                // bytes this change wrote into the save are not written.
                Redo::Transfer(req) => deals.push(self.live_transfer(req, &before)?),
                Redo::Loan(req) => deals.push(self.live_loan(req)?),
                _ => {
                    for (offset, old) in &op.patches {
                        if offset + old.len() > flat_end {
                            return Err(Error::new(format!(
                                "\"{}\" changes a part of the save the running game doesn't keep as it is; save it \
                                 the usual way (game on the main menu).",
                                op.description
                            )));
                        }
                        changed.extend((*offset..offset + old.len()).filter(|&o| before[o] != self.data[o]));
                    }
                }
            }
        }
        if deals.iter().any(|d: &live::Deal| d.user) && pending.iter().any(|op| matches!(op.redo, Redo::Finances { .. })) {
            return Err(Error::new(
                "A budget change and a deal that the game charges to your budget can't go into the game together. \
                 Write the budget change on its own first (cancel the deals, write, then make them again).",
            ));
        }
        let mut plan = live::Plan {
            game_date: self.game_date,
            flat: live::FlatChange::group(&before, &self.data, &changed),
            renewals: Vec::new(),
            deals,
        };
        for (player, name) in renewed {
            let label = name.unwrap_or_else(|| format!("ID {}", player.player_id));
            // Renewals need the managed club's table, so both are set.
            let (Some(table), Some(club)) = (self.contracts.as_ref(), self.managed_team) else {
                return Err(Error::new("First choose the club you manage."));
            };
            let rows = table.find(&before, &player);
            let old = rows.iter().find(|c| c.status == crate::contracts::STATUS_CONTRACTED && c.club_index() == club);
            let (Some(old), Some(new)) = (old, self.contract_of(&player)) else {
                return Err(Error::new(format!("{label}: the contract can't be read; nothing was written.")));
            };
            if (old.salary_units, old.end, old.release_units) != (new.salary_units, new.end, new.release_units) {
                plan.renewals.push(live::Renewal {
                    label,
                    player_id: player.player_id,
                    old_salary: old.salary_units,
                    old_end: old.end,
                    old_release: old.release_units,
                    salary: new.salary_units,
                    end: new.end,
                    release: new.release_units,
                    request_open: Self::request_open(&rows, club),
                    state_set: renewal_state(&before, &player).is_some(),
                });
            }
        }
        if plan.flat.is_empty() && plan.renewals.is_empty() && plan.deals.is_empty() {
            return Err(Error::new("The new changes give the same values the game already has; nothing to write."));
        }
        Ok((plan, self.history.len()))
    }

    /// When a deal agreed now completes in the game: at the next day end, or
    /// when the transfer window opens if it should wait for it.
    fn live_date(&self, wait_for_window: bool) -> Option<Date> {
        let today = self.game_date?;
        (wait_for_window && !deals::in_window(today)).then(|| deals::arrival_date(today, true))
    }

    /// A transfer the game agrees and completes itself. The new contract is
    /// the one asked for (incoming), or the one he has now (sales and moves
    /// between other clubs keep his terms, as in the save).
    fn live_transfer(&self, req: &TransferRequest, before: &[u8]) -> Result<live::Deal, Error> {
        let (Some(table), Some(mine)) = (self.contracts.as_ref(), self.managed_team) else {
            return Err(Error::new("First choose the club you manage."));
        };
        let label = req.name.clone().unwrap_or_else(|| format!("ID {}", req.player_id));
        let seller = match req.source {
            Source::Club(i) => Some(i as u32),
            Source::Others => None,
        };
        let (salary, end, release) = match (req.salary_eur, req.contract_end) {
            (Some(salary), Some(end)) if req.dest == mine => (
                u32::try_from(div_round_even(salary, SALARY_UNIT as i64)).unwrap_or(0),
                end,
                release_units(req.release_clause_eur.unwrap_or(0))?,
            ),
            _ => {
                let club = seller.ok_or_else(|| Error::new(format!("{label}: a player without a club can only join your club.")))?;
                let now = table
                    .rows(before)
                    .into_iter()
                    .find(|c| c.player_id == req.player_id && c.status == crate::contracts::STATUS_CONTRACTED && c.club_index() == club as usize)
                    .ok_or_else(|| Error::new(format!("{label}: his contract wasn't found in the save; nothing was written.")))?;
                (now.salary_units, now.end, now.release_units)
            }
        };
        Ok(live::Deal {
            kind: if seller.is_some() { live::DealKind::Transfer } else { live::DealKind::FreeAgent },
            player_id: req.player_id,
            buyer: req.dest as u32,
            seller,
            fee: u32::try_from(eur_to_units(req.fee_eur)).unwrap_or(0),
            salary,
            end,
            release,
            loan_end: None,
            date: if req.dest == mine { self.live_date(req.wait_for_window) } else { None },
            user: req.use_transfer_budget && (req.dest == mine || seller == Some(mine as u32)),
            label,
        })
    }

    /// A loan the game agrees and completes itself; his contract stays with the parent club.
    fn live_loan(&self, req: &LoanRequest) -> Result<live::Deal, Error> {
        let mine = self.managed_team.ok_or_else(|| Error::new("First choose the club you manage."))? as u32;
        let (buyer, seller) = if req.outgoing { (req.club as u32, mine) } else { (mine, req.club as u32) };
        Ok(live::Deal {
            label: req.name.clone().unwrap_or_else(|| format!("ID {}", req.player_id)),
            kind: live::DealKind::Loan,
            player_id: req.player_id,
            buyer,
            seller: Some(seller),
            fee: u32::try_from(eur_to_units(req.fee_eur)).unwrap_or(0),
            salary: 0,
            end: req.loan_end,
            release: 0,
            loan_end: Some(req.loan_end),
            date: self.live_date(req.wait_for_window),
            user: req.use_transfer_budget,
        })
    }

    /// The game has a renewal request of the managed club open for the
    /// player: a negotiation row (status 1–3) of that club in `rows`.
    fn request_open(rows: &[Contract], club: usize) -> bool {
        rows.iter().any(|c| (1..=3).contains(&c.status) && c.club_index() == club)
    }
}

/// The game's renewal state row of the player: 16 bytes `(internal id,
/// player id, state, x)` about 15 KB before the end of the data block, one
/// per player of the managed squad; 1 = renewal requested, 2 = On Hold,
/// 3 = refused (he leaves when the contract ends). `x` has its low three
/// bytes zero; its high byte is a per-player value the game keeps (FL27).
/// -> offset of the state word when it is set, and only when exactly one
/// such row exists.
fn renewal_state(data: &[u8], player: &Player) -> Option<usize> {
    let from = data.len().saturating_sub(RENEWAL_STATE_WINDOW);
    let mut key = player.internal_id.to_le_bytes().to_vec();
    key.extend(player.player_id.to_le_bytes());
    let found: Vec<usize> = memchr::memmem::find_iter(&data[from..], &key)
        .map(|i| from + i + 8)
        .filter(|&o| {
            o + 8 <= data.len() && (1..=3).contains(&u32_at(data, o)) && u32_at(data, o + 4) & STATE_ROW_ZERO_MASK == 0
        })
        .collect();
    (found.len() == 1).then(|| found[0])
}

impl MasterLeague {
    /// The first `count` changes are now in the running game.
    pub fn mark_written_to_game(&mut self, count: usize) {
        self.live_written = self.live_written.max(count.min(self.history.len()));
    }

    fn shirt_numbers(&self, index: usize) -> Vec<u16> {
        let base = Self::base(index) + NUMBERS_OFFSET;
        (0..MAX_ROSTER).map(|j| u16_at(&self.data, base + 2 * j)).collect()
    }

    /// Squad and shirt numbers (same order; zeros after the last player), as the game keeps them.
    fn write_squad(&mut self, index: usize, players: &[Player], numbers: &[u16]) -> Result<(), Error> {
        if players.len() > MAX_ROSTER || numbers.len() != players.len() {
            return Err(Error::new("The destination squad would go over 40 players."));
        }
        let base = Self::base(index);
        let mut bytes = Vec::with_capacity(MAX_ROSTER * 8);
        for p in players.iter().chain(std::iter::repeat_n(&EMPTY, MAX_ROSTER - players.len())) {
            bytes.extend(p.internal_id.to_le_bytes());
            bytes.extend(p.player_id.to_le_bytes());
        }
        self.patch(base + ROSTER_OFFSET, &bytes);
        let numbers: Vec<u8> =
            numbers.iter().chain(std::iter::repeat_n(&0, MAX_ROSTER - numbers.len())).flat_map(|n| n.to_le_bytes()).collect();
        self.patch(base + NUMBERS_OFFSET, &numbers);
        let word = u16_at(&self.data, base + COUNT_OFFSET);
        self.patch(base + COUNT_OFFSET, &((word & !COUNT_MASK) | players.len() as u16).to_le_bytes());
        Ok(())
    }

    /// Take the player out of a club's squad; later players and their shirt
    /// numbers move up. Returns his shirt number.
    fn remove_from_squad(&mut self, index: usize, player: &Player) -> Result<u16, Error> {
        let mut squad = self.club_roster(index);
        let mut numbers = self.shirt_numbers(index);
        numbers.truncate(squad.len());
        let k = squad.iter().position(|p| p == player).ok_or_else(|| Error::new("Player not found at his club."))?;
        squad.remove(k);
        let number = numbers.remove(k);
        self.write_squad(index, &squad, &numbers)?;
        Ok(number)
    }

    /// Add the player at the end of a club's squad with his old shirt number
    /// when it is free there (as the game does), otherwise the lowest free one.
    fn add_to_squad(&mut self, index: usize, player: Player, number: Option<u16>) -> Result<u16, Error> {
        let mut squad = self.club_roster(index);
        let mut numbers = self.shirt_numbers(index);
        numbers.truncate(squad.len());
        let taken: HashSet<u16> = numbers.iter().copied().collect();
        let number = number
            .filter(|n| (1..=99).contains(n) && !taken.contains(n))
            .or_else(|| (1..=99).find(|n| !taken.contains(n)))
            .ok_or_else(|| Error::new(format!("{} has no free shirt number.", self.teams[index].name)))?;
        squad.push(player);
        numbers.push(number);
        self.write_squad(index, &squad, &numbers)?;
        Ok(number)
    }

    /// Remove the player's row from the managed club's "My Team" list (later
    /// rows move up), as the list holds exactly the squad plus loans. Lists
    /// of another club, or without the player, are left alone.
    fn drop_from_my_team(&mut self, club: usize, player: &Player) {
        let Some(layout) = self.layout else { return };
        let (club_at, first) = (layout.middle(MY_TEAM_CLUB), layout.middle(MY_TEAM));
        let Some(end) = first.checked_add(MY_TEAM_ROWS * MY_TEAM_ROW).filter(|e| *e <= self.data.len()) else { return };
        if u32_at(&self.data, club_at) != u32_at(&self.data, Self::base(club)) {
            return;
        }
        let row = |k: usize| first + k * MY_TEAM_ROW;
        let used = (0..MY_TEAM_ROWS).take_while(|&k| u32_at(&self.data, row(k) + 12) & 0xFF != MY_TEAM_FREE).count();
        let found = (0..used).find(|&k| {
            u32_at(&self.data, row(k)) == player.internal_id && u32_at(&self.data, row(k) + 4) == player.player_id
        });
        let Some(k) = found else { return };
        let mut bytes = self.data[row(k + 1)..row(used)].to_vec();
        bytes.extend([0xFFFFu32, 0, 0, MY_TEAM_FREE].iter().flat_map(|w| w.to_le_bytes()));
        debug_assert!(row(k) + bytes.len() <= end);
        self.patch(row(k), &bytes);
    }

    /// Rewrite the player's record with `change`.
    fn update_record(&mut self, player: &Player, change: impl FnOnce(&[u8]) -> Result<Vec<u8>, Error>) -> Result<(), Error> {
        let db = self.players.as_ref().ok_or_else(|| Error::new(self.players_error.clone().unwrap_or_default()))?;
        let off = db.record(&self.records, player)?;
        let new = change(&self.records[off..off + playerdb::RECORD])?;
        self.patch_record(off, &new);
        Ok(())
    }

    // --- finances -----------------------------------------------------------
    fn finance_at(&self) -> Result<usize, Error> {
        self.finance_offset.ok_or_else(|| Error::new(self.finance_error.clone().unwrap_or_default()))
    }

    pub fn finances(&self) -> Result<Finances, Error> {
        let off = self.finance_at()?;
        Ok(Finances {
            transfer_eur: self.transfer_units(off) * MONEY_UNIT,
            salary_eur: u32_at(&self.data, off + SALARY_BUDGET_OFFSET) as i64 * MONEY_UNIT,
        })
    }

    fn set_u32(&mut self, offset: usize, value: i64, label: &str) -> Result<(), Error> {
        let value = u32::try_from(value)
            .ok()
            .filter(|v| *v < 0x8000_0000)
            .ok_or_else(|| Error::new(format!("{label} is too high or too low for the game.")))?;
        self.patch(offset, &value.to_le_bytes());
        Ok(())
    }

    /// The transfer budget in units of 100 EUR. Signed: the game can take it
    /// below zero (compensation paid in the game).
    fn transfer_units(&self, off: usize) -> i64 {
        u32_at(&self.data, off) as i32 as i64
    }

    /// Write the transfer budget, and the copy in your club's record when it
    /// holds the same value: after a deal accepted in the game or a change of
    /// club the game keeps the budget there too and takes it back from there,
    /// so a change to the finance block alone would be lost (The Gaffer,
    /// pc512s). A copy with another value is left alone.
    fn set_transfer_units(&mut self, off: usize, units: i64) -> Result<(), Error> {
        let old = self.transfer_units(off);
        let value = i32::try_from(units).map_err(|_| Error::new("Transfer budget is too high or too low for the game."))?;
        let copy = self.managed_team.map(Self::base).filter(|&b| {
            u32_at(&self.data, b + CLUB_USER_MARK) == USER_CLUB && u32_at(&self.data, b + CLUB_BUDGET) as i32 as i64 == old
        });
        self.patch(off, &value.to_le_bytes());
        if let Some(b) = copy {
            self.patch(b + CLUB_BUDGET, &value.to_le_bytes());
        }
        Ok(())
    }

    /// Transfer budget += `delta_eur`. A payment may not take it below zero
    /// (or lower when it already is); income is always added.
    fn adjust_transfer_budget(&mut self, delta_eur: i64) -> Result<(), Error> {
        let off = self.finance_at()?;
        let old = self.transfer_units(off);
        let delta = div_round_even(delta_eur, MONEY_UNIT);
        let units = old + delta;
        if delta < 0 && units < 0 {
            // Already below zero: the whole payment is missing.
            let short = if old < 0 { -delta } else { -units };
            return Err(Error::new(format!("Not enough transfer budget: {} short.", fmt_eur(short * MONEY_UNIT))));
        }
        self.set_transfer_units(off, units.min(i32::MAX as i64))
    }

    pub fn set_finances(&mut self, transfer_eur: i64, salary_eur: i64) -> Result<&Operation, Error> {
        let before = self.finances()?;
        let off = self.finance_at()?;
        if transfer_eur == before.transfer_eur && salary_eur == before.salary_eur {
            return Err(Error::new("Nothing was changed."));
        }
        self.atomic(Redo::Finances { transfer_eur, salary_eur }, move |g| {
            let mut changes = Vec::new();
            if transfer_eur != before.transfer_eur {
                if transfer_eur < 0 {
                    return Err(Error::new("Transfer budget is too high or too low for the game."));
                }
                g.set_transfer_units(off, eur_to_units(transfer_eur))?;
                changes.push(format!("transfer budget {} → {}", fmt_eur(before.transfer_eur), fmt_eur(transfer_eur)));
            }
            if salary_eur != before.salary_eur {
                g.set_u32(off + SALARY_BUDGET_OFFSET, eur_to_units(salary_eur), "Salary budget")?;
                changes.push(format!("salary budget {} → {}", fmt_eur(before.salary_eur), fmt_eur(salary_eur)));
            }
            Ok(format!("Finances: {}", changes.join("; ")))
        })
    }

    // --- manager name -------------------------------------------------------
    pub fn manager_name(&self) -> Result<String, Error> {
        match &self.manager_error {
            Some(e) => Err(Error::new(e.clone())),
            None => Ok(text_at(&self.data, self.manager_names[0], MANAGER_NAME_SIZE)),
        }
    }

    pub fn set_manager_name(&mut self, manager_name: &str) -> Result<&Operation, Error> {
        let before = self.manager_name()?;
        let name = manager_name.trim();
        if name.is_empty() || name.len() >= MANAGER_NAME_SIZE {
            return Err(Error::new(format!(
                "Enter a manager name of at most {} letters (fewer with accents or special characters).",
                MANAGER_NAME_SIZE - 1
            )));
        }
        if manager_name.chars().any(|c| c < ' ') {
            return Err(Error::new("The manager name contains characters that aren't allowed."));
        }
        if name == before {
            return Err(Error::new("Nothing was changed."));
        }
        let name = name.to_string();
        self.atomic(Redo::Manager(name.clone()), move |g| {
            let mut bytes = name.as_bytes().to_vec();
            bytes.resize(MANAGER_NAME_SIZE, 0);
            for offset in g.manager_names.clone() {
                g.patch(offset, &bytes);
            }
            Ok(format!("Manager: \"{before}\" → \"{name}\""))
        })
    }

    // --- managed club -----------------------------------------------------
    /// Select the club the user manages; returns the number of players under contract.
    pub fn set_managed_team(&mut self, index: usize) -> Result<usize, Error> {
        if !self.history.is_empty() {
            return Err(Error::new("Save or undo your changes before switching clubs."));
        }
        if index >= self.teams.len() {
            return Err(Error::new("Choose the club you manage."));
        }
        let roster = self.club_roster(index);
        let table = ContractTable::detect(&self.data, index, &roster)
            .map_err(|e| Error::new(format!("Couldn't read the contracts of {}: {e}", self.teams[index].name)))?;
        let count = table.contracts(&self.data, index).len();
        self.managed_team = Some(index);
        self.contracts = Some(table);
        Ok(count)
    }

    /// The player's contract with the managed club.
    pub fn contract_of(&self, player: &Player) -> Option<Contract> {
        let (table, club) = (self.contracts.as_ref()?, self.managed_team?);
        let found: Vec<Contract> = table
            .find(&self.data, player)
            .into_iter()
            .filter(|c| c.status == crate::contracts::STATUS_CONTRACTED && c.club_index() == club)
            .collect();
        (found.len() == 1).then(|| found[0])
    }

    pub fn contract_count(&self) -> usize {
        match (&self.contracts, self.managed_team) {
            (Some(t), Some(club)) => t.contracts(&self.data, club).len(),
            _ => 0,
        }
    }

    /// Annual wages of the managed club's contracts, in EUR.
    pub fn wage_bill_eur(&self) -> i64 {
        match (&self.contracts, self.managed_team) {
            (Some(t), Some(club)) => t.wage_bill_units(&self.data, club) as i64 * SALARY_UNIT as i64,
            _ => 0,
        }
    }

    /// Annual wages the game subtracts from the salary budget, in EUR: the
    /// squad's contracts plus agreed signings that haven't arrived yet.
    pub fn committed_wages_eur(&self) -> i64 {
        match (&self.contracts, self.managed_team) {
            (Some(t), Some(club)) => t.committed_wage_units(&self.data, club) as i64 * SALARY_UNIT as i64,
            _ => 0,
        }
    }

    /// Same month/day as the club's existing contracts, `seasons` years after the game date.
    pub fn contract_end_in(&self, seasons: u16) -> Date {
        let year = self.game_date.map(|d| d.year).unwrap_or_else(|| chrono::Datelike::year(&chrono::Local::now()) as u16);
        let (month, day) = match (&self.contracts, self.managed_team) {
            (Some(t), Some(club)) => t.contracts(&self.data, club).first().map_or((6, 30), |c| (c.end.month, c.end.day)),
            _ => (6, 30),
        };
        let year = (year + seasons).min(2150);
        Date::new(year, month, day).unwrap_or(Date { year, month: 6, day: 30 })
    }

    // --- transfers --------------------------------------------------------
    pub fn transfer(&mut self, req: TransferRequest) -> Result<&Operation, Error> {
        if let Source::Club(i) = req.source
            && i >= self.teams.len()
        {
            return Err(Error::new("The player's current club isn't valid."));
        }
        if req.dest >= self.teams.len() {
            return Err(Error::new("The destination must be a real club, not a national team."));
        }
        if req.source == Source::Club(req.dest) {
            return Err(Error::new("From and to are the same club."));
        }
        if req.fee_eur < 0 {
            return Err(Error::new("The fee can't be negative."));
        }
        let mine = self.managed_team;
        let incoming = Some(req.dest) == mine;
        let outgoing = matches!(req.source, Source::Club(i) if Some(i) == mine);
        if (incoming || outgoing) && self.contracts.is_none() {
            return Err(Error::new("First choose the club you manage."));
        }
        if (incoming || outgoing)
            && req.use_transfer_budget
            && req.fee_eur != 0
            && let Some(e) = &self.finance_error
        {
            return Err(Error::new(format!("{e} Untick \"Pay fee from transfer budget\" to continue.")));
        }
        let src_roster = self.roster(req.source);
        let dst_roster = self.club_roster(req.dest);
        let chosen: Vec<Player> = src_roster.iter().copied().filter(|p| p.player_id == req.player_id).collect();
        if chosen.len() != 1 {
            return Err(Error::new("Player not found (or listed twice) at their current club."));
        }
        let player = chosen[0];
        if dst_roster.len() >= MAX_ROSTER {
            return Err(Error::new(format!("{} already has 40 players.", self.teams[req.dest].name)));
        }
        if dst_roster.iter().any(|p| p.player_id == req.player_id) {
            return Err(Error::new("The player is already at the destination club."));
        }
        let elsewhere: Vec<String> = self
            .club_of(req.player_id)
            .into_iter()
            .filter(|&i| Source::Club(i) != req.source)
            .take(4)
            .map(|i| self.teams[i].name.clone())
            .collect();
        if !elsewhere.is_empty() {
            return Err(Error::new(format!("The player is also listed at another club: {}", elsewhere.join(", "))));
        }
        if incoming && (req.salary_eur.is_none() || req.contract_end.is_none()) {
            return Err(Error::new("Enter a salary and contract end date for a player joining your club."));
        }
        let Some(today) = self.game_date else {
            return Err(Error::new("Couldn't read the in-game date from the save, so nothing was changed."));
        };
        if outgoing {
            self.refuse_agreed_deal(self.contracts.as_ref().unwrap(), &player)?;
        }
        let label = req.name.clone().unwrap_or_else(|| format!("ID {}", req.player_id));
        let header = format!("{label}: {} → {}", self.source_name(req.source), self.teams[req.dest].name);
        self.atomic(Redo::Transfer(req.clone()), move |g| {
            let mut parts = vec![header];
            let mut new_contract = None;
            if outgoing {
                let table = g.contracts.clone().unwrap();
                let (offset, bytes, contract) = table.removal(&g.data, &player, mine.unwrap())?;
                g.patch(offset, &bytes);
                parts.push(format!("contract of {}/yr removed", fmt_eur(contract.annual_eur() as i64)));
                if req.use_transfer_budget && req.fee_eur != 0 {
                    g.adjust_transfer_budget(req.fee_eur)?;
                    parts.push(format!("fee received {}", fmt_eur(req.fee_eur)));
                }
            }
            if incoming {
                let (salary_eur, end) = (req.salary_eur.unwrap(), req.contract_end.unwrap());
                if req.use_transfer_budget && req.fee_eur != 0 {
                    g.adjust_transfer_budget(-req.fee_eur)?;
                    parts.push(format!("fee {}", fmt_eur(req.fee_eur)));
                }
                let units = u32::try_from(div_round_even(salary_eur, SALARY_UNIT as i64)).unwrap_or(0);
                let fee = u32::try_from(eur_to_units(req.fee_eur)).unwrap_or(0);
                let release = release_units(req.release_clause_eur.unwrap_or(0))?;
                let salary = fmt_eur(units as i64 * SALARY_UNIT as i64);
                let table = g.contracts.clone().unwrap();
                // An agreed deal: the game moves the player on the next day,
                // or (when asked to wait) when the next transfer window opens.
                // Saves whose transfer lists aren't recognised join right away,
                // and refuse a closed window when asked to wait.
                match DealTables::locate(&g.data, &table, g.layout) {
                    Ok(tables) => {
                        let arrival = deals::arrival_date(today, req.wait_for_window);
                        let deal = AgreedDeal {
                            player,
                            buyer_ref: table.team_ref,
                            seller_ref: match req.source {
                                Source::Club(seller) => Some(u32_at(&g.data, Self::base(seller))),
                                Source::Others => None,
                            },
                            fee_units: fee,
                            salary_units: units,
                            release_units: release,
                            end,
                            today,
                            arrival,
                        };
                        for (offset, bytes) in tables.agree(&g.data, &table, &deal)? {
                            g.patch(offset, &bytes);
                        }
                        if !table.find(&g.data, &player).iter().any(|c| c.status == STATUS_AGREED && c.team == table.team_ref) {
                            return Err(Error::new("Final check failed: the agreed deal can't be read."));
                        }
                        parts.push(format!("deal agreed, arrives {arrival}; salary {salary}/yr until {end}"));
                        return Ok(parts.join("; "));
                    }
                    Err(_) if req.wait_for_window && !deals::in_window(today) => {
                        return Err(Error::new(
                            "The transfer window is closed and this save's transfer lists weren't recognised, so the \
                             signing can't wait for the window. Untick \"Wait for transfer window\" to sign him now.",
                        ));
                    }
                    Err(_) => {
                        let (offset, record) = table.signing(&g.data, &player, units, release, end, today)?;
                        g.patch(offset, &record);
                        new_contract = Some((units, end));
                        parts.push(format!("salary {salary}/yr until {end}"));
                        parts.push("joined right away (this save's transfer lists weren't recognised)".into());
                    }
                }
            }
            // The editor moves the player itself, so it also does what the game
            // does on a move: his record, the shirt numbers and "My Team".
            let from = match req.source {
                Source::Club(i) => Some(u32_at(&g.data, Self::base(i))),
                Source::Others => None,
            };
            g.update_record(&player, |r| playerdb::moved(r, &player, &Move { from, joined: today, contract: new_contract }))?;
            let number = match req.source {
                Source::Club(i) => Some(g.remove_from_squad(i, &player)?),
                Source::Others => None,
            };
            let number = g.add_to_squad(req.dest, player, number)?;
            if outgoing {
                g.drop_from_my_team(mine.unwrap(), &player);
            }
            parts.push(format!("shirt number {number}"));
            if let Source::Club(i) = req.source
                && g.club_roster(i).contains(&player)
            {
                return Err(Error::new("Final check failed: the player is still at the old club."));
            }
            if !g.club_roster(req.dest).contains(&player) {
                return Err(Error::new("Final check failed: the player didn't join the new club."));
            }
            if incoming && g.contract_of(&player).is_none() {
                return Err(Error::new("Final check failed: the new contract can't be read."));
            }
            Ok(parts.join("; "))
        })
    }

    /// Managed club, its contract table and the in-game date, needed to edit contracts.
    fn contract_context(&self) -> Result<(usize, ContractTable, Date), Error> {
        let (Some(club), Some(table)) = (self.managed_team, self.contracts.clone()) else {
            return Err(Error::new("First choose the club you manage."));
        };
        let today = self.game_date.ok_or_else(|| Error::new("Couldn't read the in-game date from the save."))?;
        Ok((club, table, today))
    }

    /// Refuses a player the game already has an agreed deal for (a sale or loan waiting for its arrival date).
    fn refuse_agreed_deal(&self, table: &ContractTable, player: &Player) -> Result<(), Error> {
        match DealTables::locate(&self.data, table, self.layout) {
            Ok(tables) if tables.has_agreed_offer(&self.data, player) => Err(Error::new(format!(
                "Player ID {} already has an agreed deal (for example a loan) that the game completes on its arrival date.",
                player.player_id
            ))),
            _ => Ok(()),
        }
    }

    /// Agree a loan to or from the managed club. As in the game, the fee is
    /// paid (or received) now and the player moves on the next in-game day,
    /// or when the next transfer window opens if the request waits for it;
    /// his contract stays with the club that loans him out.
    pub fn loan(&mut self, req: LoanRequest) -> Result<&Operation, Error> {
        let (mine, table, today) = self.contract_context()?;
        if req.club >= self.teams.len() || req.club == mine {
            return Err(Error::new("Choose another club (not a national team) for the loan."));
        }
        if req.fee_eur < 0 {
            return Err(Error::new("The fee can't be negative."));
        }
        let pay = req.use_transfer_budget && req.fee_eur != 0;
        if pay && let Some(e) = &self.finance_error {
            return Err(Error::new(format!("{e} Untick \"Pay fee from transfer budget\" to continue.")));
        }
        let fee_units = u32::try_from(eur_to_units(req.fee_eur))
            .ok()
            .filter(|u| *u < 0x8000_0000)
            .ok_or_else(|| Error::new("The fee is too high for the game."))?;
        let (from, to) = if req.outgoing { (mine, req.club) } else { (req.club, mine) };
        let chosen: Vec<Player> = self.club_roster(from).into_iter().filter(|p| p.player_id == req.player_id).collect();
        if chosen.len() != 1 {
            return Err(Error::new("Player not found (or listed twice) at their current club."));
        }
        let player = chosen[0];
        if self.club_of(req.player_id) != [from] {
            return Err(Error::new("The player is listed at more than one club."));
        }
        if self.club_roster(to).len() >= MAX_ROSTER {
            return Err(Error::new(format!("{} already has 40 players.", self.teams[to].name)));
        }
        if req.outgoing {
            if self.contract_of(&player).is_none() {
                return Err(Error::new("Only players under contract with your club can be loaned out (not players on loan)."));
            }
            self.refuse_agreed_deal(&table, &player)?;
        } else if !table.find(&self.data, &player).is_empty() {
            return Err(Error::new(format!(
                "Player ID {} is already in a negotiation or under contract in the game; finish or cancel that there first.",
                player.player_id
            )));
        }
        let tables = DealTables::locate(&self.data, &table, self.layout)?;
        let club_ref = |i: usize| if i == mine { table.team_ref } else { u32_at(&self.data, Self::base(i)) };
        let arrival = deals::arrival_date(today, req.wait_for_window);
        let deal = LoanDeal {
            player,
            buyer_ref: club_ref(to),
            seller_ref: club_ref(from),
            outgoing: req.outgoing,
            fee_units,
            loan_end: req.loan_end,
            today,
            arrival,
        };
        let label = req.name.clone().unwrap_or_else(|| format!("ID {}", req.player_id));
        let header = format!(
            "{label}: loan {} → {} until {}, moves {arrival}",
            self.teams[from].name, self.teams[to].name, req.loan_end
        );
        self.atomic(Redo::Loan(req.clone()), move |g| {
            for (offset, bytes) in tables.agree_loan(&g.data, &table, &deal)? {
                g.patch(offset, &bytes);
            }
            let mut parts = vec![header];
            if pay {
                g.adjust_transfer_budget(if req.outgoing { req.fee_eur } else { -req.fee_eur })?;
                parts.push(format!("fee {} {}", if req.outgoing { "received" } else { "paid" }, fmt_eur(req.fee_eur)));
            }
            if !DealTables::locate(&g.data, &table, g.layout)?.has_agreed_offer(&g.data, &player) {
                return Err(Error::new("Final check failed: the agreed loan can't be read."));
            }
            Ok(parts.join("; "))
        })
    }

    /// The managed club's squad with each player's contract (`None` when it can't be read).
    pub fn squad_contracts(&self) -> Vec<(Player, Option<Contract>)> {
        let Some(club) = self.managed_team else { return Vec::new() };
        self.club_roster(club).into_iter().map(|p| (p, self.contract_of(&p))).collect()
    }

    /// Players under contract with the managed club who play somewhere else
    /// on loan: their contract stays with you, their roster slot is the other
    /// club's (`None` when no club roster holds them).
    pub fn loaned_out(&self) -> Vec<(Player, Contract, Option<usize>)> {
        let (Some(table), Some(club)) = (self.contracts.as_ref(), self.managed_team) else { return Vec::new() };
        let squad: HashSet<Player> = self.club_roster(club).into_iter().collect();
        let away: Vec<Contract> = table
            .contracts(&self.data, club)
            .into_iter()
            .filter(|c| !squad.contains(&Player { internal_id: c.internal_id, player_id: c.player_id }))
            .collect();
        if away.is_empty() {
            return Vec::new();
        }
        let mut at: BTreeMap<Player, usize> = BTreeMap::new();
        for i in (0..self.teams.len()).filter(|&i| i != club) {
            for p in self.club_roster(i) {
                at.entry(p).or_insert(i);
            }
        }
        away.into_iter()
            .map(|c| {
                let p = Player { internal_id: c.internal_id, player_id: c.player_id };
                (p, c, at.get(&p).copied())
            })
            .collect()
    }

    /// Loan details from the player's record: the loan end, and the club
    /// he came from this season. `None` when he isn't on loan (or his
    /// record can't be read).
    pub fn loan_of(&self, player: &Player) -> Option<LoanInfo> {
        let db = self.players.as_ref()?;
        let off = db.record(&self.records, player).ok()?;
        let r = &self.records[off..off + playerdb::RECORD];
        let end = u32_at(r, playerdb::R_LOAN_END);
        if r[playerdb::R_FLAGS] & playerdb::FLAG_LOAN == 0 && end == playerdb::NO_LOAN {
            return None;
        }
        let from_ref = u32_at(r, playerdb::R_FROM_CLUB);
        let from = (from_ref != playerdb::NO_CLUB)
            .then_some((from_ref & 0x3FFF) as usize)
            .filter(|&i| i < self.teams.len() && self.club_ref(i) == from_ref);
        Some(LoanInfo { until: (end != playerdb::NO_LOAN).then(|| Date::unpack(end)).flatten(), from })
    }

    /// New yearly salary, end date and (when given; 0 = none) release clause
    /// for a player under contract with the managed club. When the game has
    /// a renewal request open for him, the change answers it and can only go
    /// into the running game (`live`).
    pub fn renew_contract(
        &mut self,
        player: Player,
        salary_eur: i64,
        end: Date,
        release_eur: Option<i64>,
        name: Option<&str>,
    ) -> Result<&Operation, Error> {
        let (club, table, today) = self.contract_context()?;
        if salary_eur < 0 {
            return Err(Error::new("The salary can't be negative."));
        }
        let units = u32::try_from(div_round_even(salary_eur, SALARY_UNIT as i64)).unwrap_or(u32::MAX);
        let release = release_eur.map(release_units).transpose()?;
        let old = self.contract_of(&player).ok_or_else(|| Error::new("This player's contract couldn't be found; nothing was changed."))?;
        if old.salary_units == units && old.end == end && release.is_none_or(|r| r == old.release_units) {
            return Err(Error::new("Nothing was changed."));
        }
        let request_open = Self::request_open(&table.find(&self.data, &player), club);
        let state = renewal_state(&self.data, &player);
        let label = name.map_or_else(|| format!("ID {}", player.player_id), str::to_string);
        let redo = Redo::Renew { player, salary_eur, end, release_eur, name: name.map(str::to_string) };
        self.atomic(redo, move |g| {
            let (offset, bytes, _) = table.renewal(&g.data, &player, club, units, end, release, today)?;
            g.patch(offset, &bytes);
            // The player's record mirrors the contract's salary and end date.
            g.update_record(&player, |r| Ok(playerdb::renewed(r, units, end)))?;
            // As after "Accept Terms" in the game: no renewal request open, no
            // longer leaving when the contract ends.
            if let Some(o) = state {
                g.patch(o, &0u32.to_le_bytes());
            }
            g.current.as_mut().unwrap().live_only = request_open;
            let new = g.contract_of(&player).filter(|c| {
                c.salary_units == units && c.end == end && release.is_none_or(|r| r == c.release_units)
            });
            let Some(new) = new else {
                return Err(Error::new("Final check failed: the new contract can't be read."));
            };
            let clause = |u: u32| if u == 0 { "none".to_string() } else { fmt_eur(u as i64 * SALARY_UNIT as i64) };
            let mut text = format!(
                "{label}: contract {}/yr until {} → {}/yr until {end}",
                fmt_eur(old.annual_eur() as i64),
                old.end,
                fmt_eur(units as i64 * SALARY_UNIT as i64)
            );
            if new.release_units != old.release_units {
                text += &format!("; release clause {} → {}", clause(old.release_units), clause(new.release_units));
            }
            if request_open {
                text += " (answers the game's renewal request: running game only)";
            }
            Ok(text)
        })
    }

    /// End a player's contract with the managed club and take him out of the
    /// squad, without a new club. `compensation_eur` is paid from the transfer
    /// budget when `use_transfer_budget` is set.
    pub fn release(
        &mut self,
        player: Player,
        compensation_eur: i64,
        use_transfer_budget: bool,
        name: Option<&str>,
    ) -> Result<&Operation, Error> {
        let (club, table, _) = self.contract_context()?;
        if compensation_eur < 0 {
            return Err(Error::new("The compensation can't be negative."));
        }
        let pay = use_transfer_budget && compensation_eur != 0;
        if pay && let Some(e) = &self.finance_error {
            return Err(Error::new(format!("{e} Untick \"Pay from transfer budget\" to continue.")));
        }
        let roster = self.club_roster(club);
        if roster.iter().filter(|p| **p == player).count() != 1 {
            return Err(Error::new("Player not found (or listed twice) in your squad."));
        }
        self.refuse_agreed_deal(&table, &player)?;
        let label = name.map_or_else(|| format!("ID {}", player.player_id), str::to_string);
        let redo = Redo::Release { player, compensation_eur, use_transfer_budget, name: name.map(str::to_string) };
        self.atomic(redo, move |g| {
            let (offset, bytes, contract) = table.removal(&g.data, &player, club)?;
            g.patch(offset, &bytes);
            let mut parts = vec![
                format!("{label}: released from {}", g.teams[club].name),
                format!("contract of {}/yr until {} removed", fmt_eur(contract.annual_eur() as i64), contract.end),
            ];
            if pay {
                g.adjust_transfer_budget(-compensation_eur)?;
                parts.push(format!("compensation {}", fmt_eur(compensation_eur)));
            }
            g.update_record(&player, |r| playerdb::released(r, &player))?;
            g.remove_from_squad(club, &player)?;
            g.drop_from_my_team(club, &player);
            if g.club_roster(club).contains(&player) || g.contract_of(&player).is_some() {
                return Err(Error::new("Final check failed: the player is still at your club."));
            }
            Ok(parts.join("; "))
        })
    }

    // --- league results ---------------------------------------------------
    /// A club's reference, as the match and league tables store it.
    fn club_ref(&self, index: usize) -> u32 {
        u32_at(&self.data, Self::base(index))
    }

    /// Name of the club with this reference (low 14 bits = club index).
    pub fn club_name(&self, reference: u32) -> String {
        let index = (reference & 0x3FFF) as usize;
        match self.teams.get(index) {
            Some(t) if self.club_ref(index) == reference => t.name.clone(),
            _ => format!("Club {reference:#x}"),
        }
    }

    fn managed_league(&self) -> Result<(usize, u32, League), Error> {
        let club = self.managed_team.ok_or_else(|| Error::new("First choose the club you manage."))?;
        let me = self.club_ref(club);
        Ok((club, me, League::of(&self.data, me)?))
    }

    fn league_label(&self, club: usize) -> String {
        self.league_name.clone().filter(|_| self.described_club == Some(club)).unwrap_or_else(|| "League".into())
    }

    /// The managed club's league matches and its table.
    pub fn league(&self) -> Result<LeagueView, Error> {
        let (club, me, league) = self.managed_league()?;
        Ok(LeagueView {
            name: self.league_label(club),
            club: me,
            fixtures: league.fixtures_of(&self.data, me),
            standings: league.table.standings(&self.data),
        })
    }

    /// Turn a played league match of the managed club into a win with this
    /// score; the league table is recalculated. Scorers, player stats and
    /// everything else the game did after the match stay as they are.
    pub fn set_league_win(&mut self, index: usize, our_goals: u8, their_goals: u8) -> Result<&Operation, Error> {
        let (club, me, league) = self.managed_league()?;
        let f = league
            .fixtures_of(&self.data, me)
            .into_iter()
            .find(|f| f.index == index)
            .ok_or_else(|| Error::new("This isn't one of your club's league matches."))?;
        if !f.played {
            return Err(Error::new("This match hasn't been played yet: play it in the game first, then make it a win here."));
        }
        if our_goals <= their_goals {
            return Err(Error::new("Choose a winning score: your goals must be more than the opponent's."));
        }
        let (home, away) = if f.home == me { (our_goals, their_goals) } else { (their_goals, our_goals) };
        if (home, away) == (f.home_goals, f.away_goals) {
            return Err(Error::new("Nothing was changed."));
        }
        let patches = league.new_score(&self.data, index, home, away)?;
        let label = self.league_label(club);
        self.atomic(Redo::LeagueWin { index, our_goals, their_goals }, move |g| {
            for (offset, bytes) in &patches {
                g.patch(*offset, bytes);
            }
            let after = league.matches.fixture(&g.data, index);
            if (after.home_goals, after.away_goals) != (home, away) || !after.played {
                return Err(Error::new("Final check failed: the new score can't be read."));
            }
            Ok(format!(
                "{label} round {}: {} {}–{} {} → {home}–{away}",
                f.round,
                g.club_name(f.home),
                f.home_goals,
                f.away_goals,
                g.club_name(f.away)
            ))
        })
    }

    // --- saving -----------------------------------------------------------
    /// True when every byte that differs from the opened file lies in a journaled patch.
    fn changes_are_journaled(&self) -> bool {
        let mask = |current: &[u8], original: &[u8], patches: &mut dyn Iterator<Item = &(usize, Vec<u8>)>| {
            let mut masked = current.to_vec();
            for (offset, old) in patches {
                masked[*offset..offset + old.len()].copy_from_slice(&original[*offset..offset + old.len()]);
            }
            masked == original
        };
        let data_ok = mask(&self.data, &self.save.data, &mut self.history.iter().flat_map(|op| &op.patches));
        let records_ok = match &self.players {
            Some(db) => mask(&self.records, &db.records, &mut self.history.iter().flat_map(|op| &op.record_patches)),
            None => self.records.is_empty(),
        };
        data_ok && records_ok
    }

    pub fn build(&self) -> Result<Vec<u8>, Error> {
        self.build_save().map(|(out, _)| out)
    }

    /// The encrypted file and the save it decrypts to. Changed player records
    /// are compressed back into their container, which can change its size.
    fn build_save(&self) -> Result<(Vec<u8>, crypto::SaveFile), Error> {
        if !self.changes_are_journaled() {
            return Err(Error::new("Unexpected changes found in the save; nothing was saved."));
        }
        if let Some(op) = self.history.iter().find(|op| op.live_only) {
            return Err(Error::new(format!(
                "\"{}\" answers a request the game has open, so it can only go into the running game (Write into \
                 running game, then save in the game). Cancel it to save the other changes to the file.",
                op.description
            )));
        }
        let mut save = self.save.clone();
        let edited = self.players.as_ref().filter(|db| db.records != self.records);
        save.data = match edited {
            Some(db) => db.splice(&self.data, &self.records)?,
            None => self.data.clone(),
        };
        let size = u32::try_from(save.data.len()).map_err(|_| Error::new("The save would be too large."))?;
        save.file_header[64..68].copy_from_slice(&size.to_le_bytes());
        let out = crypto::pack(&save);
        let check = crypto::unpack(&out, true)?;
        let spliced_ok = edited.is_none_or(|db| db.check_splice(&self.data, &check.data, &self.records));
        if check.data != save.data
            || !spliced_ok
            || check.desc != save.desc
            || check.logo != save.logo
            || check.serial != save.serial
            || check.file_header != save.file_header
        {
            return Err(Error::new("Couldn't confirm the new file is correct; nothing was written."));
        }
        Ok((out, check))
    }

    /// Write the edited save. Overwriting backs up the current file first.
    pub fn write(&mut self, target: Option<&Path>) -> Result<(PathBuf, Option<PathBuf>), Error> {
        if self.history.is_empty() {
            return Err(Error::new("There are no changes yet."));
        }
        let (out, written) = self.build_save()?;
        let target = match target {
            Some(t) => std::path::absolute(t).map_err(|e| Error::new(e.to_string()))?,
            None => self.path.clone(),
        };
        let io = |e: std::io::Error| Error::new(format!("Couldn't write the file: {e}"));
        let mut backup = None;
        if target.exists() {
            let folder = target.parent().unwrap_or(Path::new(".")).join(BACKUP_DIR);
            std::fs::create_dir_all(&folder).map_err(io)?;
            let name = target.file_name().unwrap_or_default().to_string_lossy();
            let file = folder.join(format!("{name}.{}.bak", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            std::fs::copy(&target, &file).map_err(io)?;
            backup = Some(file);
        }
        let mut tmp = target.clone().into_os_string();
        tmp.push(".tmp");
        std::fs::write(&tmp, &out).map_err(io)?;
        std::fs::rename(&tmp, &target).map_err(io)?;
        let same = target == self.path;
        if same {
            // The file on disk is now the base: tables after the player records
            // may have moved, so they are found again.
            self.data = written.data.clone();
            self.save = written;
            self.history.clear();
            self.live_written = 0;
            self.load_players();
            if let Some(club) = self.managed_team {
                self.contracts = ContractTable::detect(&self.data, club, &self.club_roster(club)).ok();
                if self.contracts.is_none() {
                    self.managed_team = None;
                }
            }
        }
        Ok((target, backup))
    }
}
