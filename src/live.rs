//! Writing changes into the running game: budget, manager name, league
//! results and contract renewals reach a Master League that is loaded in the
//! game, without going back to the main menu and loading the save again.
//!
//! What the game keeps in memory (measured by The Gaffer on FL26, FL Mod
//! Studio and FL27; same exe layout in all three):
//!
//! * Most of the save's `data` block before the player records lies in
//!   memory byte for byte as in the file, in about eleven pieces; the finance
//!   block is one of them. A change there is seen by the game at once and
//!   saved with the career. The piece is found by searching memory for the
//!   save's bytes around the change.
//! * The contract table at the end of the save is not in memory: the game
//!   builds it when saving, from one `PlayerContract` object per contract
//!   (listed by the `ContractStation` at `[exe + 0x36FCD78]`) whose terms are
//!   `key (8 bytes), end date, salary / 100, release clause / 100`. The
//!   380-byte player records (`[[exe + 0x3705E10] + 0x48]`, stride 0x17C) hold
//!   the key at +0x2C (internal id, then the player id, repeated at +0x34),
//!   the club at +0x12C, the contract end at +0x138, "has a release clause"
//!   in bit 30 of +0x144 and the salary at +0x15C, copied from the terms.
//! * A renewal request the game has open is a second `PlayerContract` of the
//!   player (state 2 or 3, state and club at its meta +4/+8, date at +0xC).
//!   Accepting it in the game sets it to 4 with today's date and the old
//!   contract to 7; the game swaps them at the end of the day.
//! * Per player of the squad, a row `(key, state, 0)` (2–3 copies in memory,
//!   also in the save) holds 1 = renewal requested, 3 = refused (he leaves
//!   when the contract ends); a renewal sets it back to 0.
//! * The in-game date is at player table + 0x1642A24 (`u16` year, `u8`
//!   month, `u8` day); on the start screen it is 65535-00-00. FL Mod Studio
//!   worlds keep it elsewhere, found by its calendar block.
//!
//! Nothing is written unless every change is found and the game still holds
//! exactly what the opened save holds there (same date, same old values):
//! a game that has moved on since it was saved is refused.

use std::collections::HashMap;
use std::sync::Mutex;

use memchr::memmem::Finder;

use crate::Error;
use crate::contracts::Date;

/// Executables the game runs as (Football Life 2027, Football Life 2026, PES 2021).
pub const EXE_NAMES: [&str; 3] = ["FL_2027.exe", "FL_2026.exe", "PES2021.exe"];

/// `[exe base + OFF_OBJ]` = Master League object, 0 before a career is loaded.
const OFF_OBJ: u64 = 0x370_5E10;
/// `[object + OFF_TABLE]` = player table.
const OFF_TABLE: u64 = 0x48;
/// Player table + OFF_DATE = in-game date.
const OFF_DATE: u64 = 0x164_2A24;
/// Where FL Mod Studio worlds keep the date instead (player table + …).
const DATE_SCAN: std::ops::Range<u64> = 0x150_0000..0x260_0000;
const DATE_SCAN_CHUNK: usize = 0x4_0000;

const RECORD: u64 = 0x17C;
const R_KEY: usize = 0x2C;
const R_PLAYER: usize = 0x30;
const R_PLAYER_AGAIN: usize = 0x34;
const R_CLUB: usize = 0x12C;
const R_END: usize = 0x138;
const R_FLAGS: usize = 0x144;
const HAS_RELEASE_CLAUSE: u32 = 1 << 30;
const R_SALARY: usize = 0x15C;
/// Records read per batch while walking the player table.
const RECORD_BATCH: u64 = 4096;
/// Safety stop for the walk (Mod Studio worlds have more than 50,000 players).
const MAX_RECORDS: u64 = 1_000_000;

/// Contract terms: key at +0, end date at +8, salary / 100 at +0xC, release clause / 100 at +0x10.
const T_END: u64 = 8;
const T_SALARY: usize = 0xC;
const T_RELEASE: u64 = 0x10;

/// `[exe base + CONTRACT_STATION]` = the game's list of contracts and deals.
const CONTRACT_STATION: u64 = 0x36F_CD78;
const STATION_NAME: &[u8] = b"ContractStation";
/// Vtable of a `PlayerContract` object (exe base + …).
const PLAYER_CONTRACT_VTABLE: u64 = 0x27F_78A0;
const MAX_CONTRACTS: u64 = 5000;
/// States of a `PlayerContract`.
const STATE_ACCEPTED: u32 = 4;
const STATE_REPLACED: u32 = 7;
const REQUEST_STATES: [u32; 2] = [2, 3];
const STATE_CONTRACTED: u32 = 5;

/// Save bytes kept on each side of a change to confirm the piece found in memory.
const CONTEXT: usize = 2048;
/// Share of the non-zero context bytes that must agree with the save.
const CONTEXT_AGREEMENT: f64 = 0.9;
/// Non-zero bytes the search pattern around a change should hold.
const NEEDLE_NONZERO: usize = 12;
const NEEDLE_MAX: usize = 512;
/// Changes closer than this are written as one range.
const GAP: usize = 32;
/// Memory is searched in pieces of this size.
const SCAN_CHUNK: usize = 16 << 20;
const MAX_HITS: usize = 64;

/// Access to the game's memory.
pub trait Memory {
    /// Fill `buf` from `addr`; false when any byte can't be read.
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool;
    fn write(&self, addr: u64, data: &[u8]) -> bool;
    /// Committed, writable data regions `(start, size)`.
    fn regions(&self) -> Vec<(u64, u64)>;
    /// Load address of the game's executable.
    fn base(&self) -> u64;
    /// Identifies this run of the game (addresses found earlier stay valid within it).
    fn process_id(&self) -> u32;
    /// New executable, writable memory in the game; `None` when refused.
    fn alloc_code(&self, size: usize) -> Option<u64>;
    /// Write into the game's own code (made writable for the write).
    fn write_code(&self, addr: u64, data: &[u8]) -> bool;
}

/// A change to the part of the save the game keeps byte for byte.
#[derive(Clone, Debug)]
pub struct FlatChange {
    /// Offset in the save's `data` block.
    pub offset: usize,
    /// Bytes as in the opened save (and so in the game).
    pub old: Vec<u8>,
    pub new: Vec<u8>,
    /// Save bytes around the change as the game has them, from `context_at`.
    pub context: Vec<u8>,
    pub context_at: usize,
}

impl FlatChange {
    /// `old`/`new` are the whole `data` block before and after the changes;
    /// `changed` lists the offsets that differ.
    pub fn group(old: &[u8], new: &[u8], changed: &[usize]) -> Vec<FlatChange> {
        let mut offsets = changed.to_vec();
        offsets.sort_unstable();
        offsets.dedup();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for o in offsets {
            match ranges.last_mut() {
                Some((_, end)) if o < *end + GAP => *end = o + 1,
                _ => ranges.push((o, o + 1)),
            }
        }
        ranges
            .into_iter()
            .map(|(a, b)| {
                let context_at = a.saturating_sub(CONTEXT);
                let context_end = (b + CONTEXT).min(old.len());
                FlatChange {
                    offset: a,
                    old: old[a..b].to_vec(),
                    new: new[a..b].to_vec(),
                    context: old[context_at..context_end].to_vec(),
                    context_at,
                }
            })
            .collect()
    }

    /// Search pattern around the change (old bytes included, so the game must
    /// still hold them): the change plus at least 32 bytes on each side,
    /// widened until it has enough non-zero bytes. -> (start in `context`, length).
    fn needle(&self) -> (usize, usize) {
        let a = self.offset - self.context_at;
        let b = a + self.old.len();
        let mut margin = 32;
        loop {
            let (s, e) = (a.saturating_sub(margin), (b + margin).min(self.context.len()));
            let nonzero = self.context[s..e].iter().filter(|&&x| x != 0).count();
            if nonzero >= NEEDLE_NONZERO || e - s >= NEEDLE_MAX || (s == 0 && e == self.context.len()) {
                return (s, e - s);
            }
            margin += 16;
        }
    }
}

/// A contract renewal for the running game. Salaries and clauses in €100.
#[derive(Clone, Debug)]
pub struct Renewal {
    /// Player name (or "ID …") for messages.
    pub label: String,
    pub player_id: u32,
    /// Contract as in the opened save (and so in the game).
    pub old_salary: u32,
    pub old_end: Date,
    pub old_release: u32,
    pub salary: u32,
    pub end: Date,
    /// 0 = no release clause.
    pub release: u32,
    /// The save has a renewal request of the club open for him: the renewal
    /// answers it, as "Accept Terms" in the game.
    pub request_open: bool,
    /// The save marks him as asked for a renewal or leaving (state row ≠ 0).
    pub state_set: bool,
}

/// How a deal moves the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DealKind {
    /// Club to club, with a new contract at the buying club.
    Transfer = 1,
    /// A player without a club signs a contract.
    FreeAgent = 2,
    /// Club to club until `loan_end`; the contract stays with the parent club.
    Loan = 3,
}

/// A transfer or loan the game agrees and completes itself (`run_requests`
/// in `stub/live_stub.c`). Clubs are team indexes; money and wages in €100.
#[derive(Clone, Debug)]
pub struct Deal {
    /// Player name (or "ID …") for messages.
    pub label: String,
    pub kind: DealKind,
    pub player_id: u32,
    pub buyer: u32,
    /// The club he is at in the save; `None` for a free agent.
    pub seller: Option<u32>,
    pub fee: u32,
    /// New contract (transfers and free agents).
    pub salary: u32,
    pub end: Date,
    pub release: u32,
    pub loan_end: Option<Date>,
    /// When the game moves him; `None` = at the next day end.
    pub date: Option<Date>,
    /// The managed club buys or sells: the game charges or credits the fee
    /// and books the wages, as when you agree a deal in the game.
    pub user: bool,
}

/// Everything to write, built from the save (`MasterLeague::live_plan`).
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// In-game date of the opened save; the game must be on the same day.
    pub game_date: Option<Date>,
    pub flat: Vec<FlatChange>,
    pub renewals: Vec<Renewal>,
    pub deals: Vec<Deal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Places in memory the save changes went to (the game may keep more than one copy).
    pub flat_copies: usize,
    pub renewals: usize,
    /// Renewals that answered a request the game had open.
    pub requests_answered: usize,
    /// Request slots the deals wait in until the game's next day end (`deal_results`).
    pub deal_slots: Vec<u32>,
}

/// Open the running game and write the plan into it. `extra_exe` adds
/// executable names to look for (for example the ones in the game folder).
pub fn write_to_game(plan: &Plan, extra_exe: &[String]) -> Result<Report, Error> {
    let game = open_game(extra_exe)?;
    apply(plan, game.as_ref())
}

/// What became of deals armed earlier (`Report::deal_slots`), per slot:
/// `None` while it waits for the game's day end, else `Ok(())` or why it
/// failed. Once all are through, their slots are handed back for new deals.
pub fn deal_results(slots: &[u32], extra_exe: &[String]) -> Result<Vec<Option<Result<(), String>>>, Error> {
    let game = open_game(extra_exe)?;
    let mem = game.as_ref();
    let HookState::Installed(ctx) = hook_state(mem)? else {
        return Err(Error::new("The game was restarted; the waiting deals are gone. Load your last save there."));
    };
    let slot_at = |i: u32| ctx + C_SLOTS + i as u64 * SLOT_SIZE;
    let results: Vec<_> = slots
        .iter()
        .map(|&i| match (read_u32(mem, slot_at(i)), read_u32(mem, slot_at(i) + S_ERROR)) {
            (Some(SLOT_ARMED), _) => None,
            (Some(SLOT_DONE), _) => Some(Ok(())),
            (Some(SLOT_FAILED), Some(e)) => Some(Err(slot_error(e).to_string())),
            _ => Some(Err("the request was lost".to_string())),
        })
        .collect();
    if results.iter().all(Option::is_some) {
        for &i in slots {
            mem.write(slot_at(i), &word(SLOT_FREE));
        }
    }
    Ok(results)
}

/// Addresses found earlier in this run of the game; each is checked again before use.
#[derive(Default)]
struct Found {
    process: u32,
    table: u64,
    date_offset: Option<u64>,
    /// Save offset of a change -> its places in memory.
    flat: HashMap<usize, Vec<u64>>,
    /// Player id -> his contract terms.
    terms: HashMap<u32, u64>,
}

static FOUND: Mutex<Option<Found>> = Mutex::new(None);

fn read_vec(mem: &dyn Memory, addr: u64, n: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0; n];
    mem.read(addr, &mut buf).then_some(buf)
}

fn read_u64(mem: &dyn Memory, addr: u64) -> Option<u64> {
    read_vec(mem, addr, 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}

fn read_u32(mem: &dyn Memory, addr: u64) -> Option<u32> {
    read_vec(mem, addr, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn not_in_ml() -> Error {
    Error::new(
        "No Master League is loaded in the game (or this game version isn't supported). Load your career in the game, \
         then try again.",
    )
}

fn date_at(raw: &[u8]) -> Option<Date> {
    Date::new(u16::from_le_bytes([raw[0], raw[1]]), raw[2], raw[3]).filter(|d| (2000..=2200).contains(&d.year))
}

/// The calendar block of FL Mod Studio worlds: season start, a small
/// counter, the previous day and today, one word each.
fn find_date(mem: &dyn Memory, table: u64) -> Option<u64> {
    let mut hits = Vec::new();
    let mut at = DATE_SCAN.start;
    while at < DATE_SCAN.end {
        if let Some(buf) = read_vec(mem, table + at - 16, DATE_SCAN_CHUNK + 16) {
            for i in (16..buf.len() - 3).step_by(4) {
                let (Some(today), Some(prev), Some(start)) =
                    (date_at(&buf[i..]), date_at(&buf[i - 4..]), date_at(&buf[i - 12..]))
                else {
                    continue;
                };
                if start <= prev && prev <= today && u32_at(&buf, i - 8) < 400 && today.year - start.year <= 1 {
                    hits.push(at - 16 + i as u64);
                }
            }
        }
        at += DATE_SCAN_CHUNK as u64;
    }
    (hits.len() == 1).then(|| hits[0])
}

/// Player table and in-game date of the loaded career.
fn career(mem: &dyn Memory, found: &mut Found) -> Result<(u64, Date), Error> {
    let obj = read_u64(mem, mem.base() + OFF_OBJ).filter(|&o| o != 0).ok_or_else(not_in_ml)?;
    let table = read_u64(mem, obj + OFF_TABLE).filter(|&t| t != 0).ok_or_else(not_in_ml)?;
    if found.process != mem.process_id() || found.table != table {
        *found = Found { process: mem.process_id(), table, ..Found::default() };
    }
    let raw = read_vec(mem, table + OFF_DATE, 4).ok_or_else(not_in_ml)?;
    if let Some(date) = date_at(&raw) {
        return Ok((table, date));
    }
    if raw == [0xFF, 0xFF, 0, 0] {
        return Err(not_in_ml());
    }
    if found.date_offset.is_none() {
        found.date_offset = find_date(mem, table);
    }
    let off = found.date_offset.ok_or_else(not_in_ml)?;
    let date = read_vec(mem, table + off, 4).and_then(|r| date_at(&r)).ok_or_else(not_in_ml)?;
    Ok((table, date))
}

/// Address of each wanted player's record in the game's player table.
fn player_records(mem: &dyn Memory, table: u64, wanted: &[u32]) -> HashMap<u32, u64> {
    let mut found = HashMap::new();
    let mut first = 0;
    while first < MAX_RECORDS && found.len() < wanted.len() {
        let mut buf = vec![0; (RECORD_BATCH * RECORD) as usize];
        let at = table + first * RECORD;
        // The table may end inside this batch: read what is there, record by record.
        let readable = if mem.read(at, &mut buf) {
            RECORD_BATCH
        } else {
            let mut n = 0;
            while n < RECORD_BATCH {
                let r = (n * RECORD) as usize;
                if !mem.read(at + n * RECORD, &mut buf[r..r + RECORD as usize]) {
                    break;
                }
                n += 1;
            }
            n
        };
        let mut valid = 0;
        for j in 0..readable {
            let r = (j * RECORD) as usize;
            let pid = u32_at(&buf, r + R_PLAYER);
            if pid == 0 || pid != u32_at(&buf, r + R_PLAYER_AGAIN) {
                continue;
            }
            valid += 1;
            if wanted.contains(&pid) {
                found.entry(pid).or_insert(at + j * RECORD);
            }
        }
        if valid == 0 || readable < RECORD_BATCH {
            break;
        }
        first += RECORD_BATCH;
    }
    found
}

/// Addresses where each needle occurs in the game's data regions (at most `MAX_HITS` each).
fn scan(mem: &dyn Memory, needles: &[Vec<u8>]) -> Vec<Vec<u64>> {
    let mut hits = vec![Vec::new(); needles.len()];
    if needles.is_empty() {
        return hits;
    }
    let finders: Vec<Finder> = needles.iter().map(Finder::new).collect();
    let overlap = needles.iter().map(Vec::len).max().unwrap_or(1) - 1;
    let mut buf = Vec::new();
    for (start, size) in mem.regions() {
        let end = start + size;
        let mut at = start;
        while at < end {
            let step = (SCAN_CHUNK as u64).min(end - at) as usize;
            let len = (step + overlap).min((end - at) as usize);
            buf.resize(len, 0);
            if mem.read(at, &mut buf) {
                for (k, finder) in finders.iter().enumerate() {
                    // A match starting in the overlap belongs to the next piece.
                    for i in finder.find_iter(&buf).take_while(|&i| i < step) {
                        if hits[k].len() < MAX_HITS {
                            hits[k].push(at + i as u64);
                        }
                    }
                }
            }
            at += step as u64;
        }
    }
    hits
}

/// The piece of memory at `delta` from the save holds this change's
/// surroundings: most non-zero context bytes agree with the save.
fn context_agrees(mem: &dyn Memory, change: &FlatChange, delta: i128) -> bool {
    let Ok(at) = u64::try_from(change.context_at as i128 + delta) else { return false };
    let Some(m) = read_vec(mem, at, change.context.len()) else { return false };
    let (mut same, mut total) = (0usize, 0usize);
    for (x, y) in m.iter().zip(&change.context) {
        if *x != 0 || *y != 0 {
            total += 1;
            same += usize::from(x == y);
        }
    }
    total == 0 || same as f64 >= CONTEXT_AGREEMENT * total as f64
}

/// The change's place at `addr` (save offset + delta) checks out.
fn flat_place_ok(mem: &dyn Memory, c: &FlatChange, addr: u64) -> bool {
    context_agrees(mem, c, addr as i128 - c.offset as i128) && read_vec(mem, addr, c.old.len()).as_deref() == Some(&c.old[..])
}

/// The terms at `addr` belong to this player and hold the save's contract.
fn terms_ok(mem: &dyn Memory, addr: u64, key: &[u8], r: &Renewal) -> bool {
    read_vec(mem, addr, T_SALARY + 4).is_some_and(|t| {
        t[..8] == key[..] && u32_at(&t, 8) == r.old_end.pack() && u32_at(&t, T_SALARY) == r.old_salary
    })
}

/// One `PlayerContract` of the game's ContractStation.
struct GameContract {
    terms: u64,
    meta: u64,
    state: u32,
    club: u32,
}

/// First non-zero pointer among `offsets` of `obj`.
fn first_ptr(mem: &dyn Memory, obj: u64, offsets: [u64; 3]) -> Option<u64> {
    offsets.iter().find_map(|o| read_u64(mem, obj + o).filter(|&p| p != 0))
}

/// The player contracts of the game's ContractStation with this key; `None`
/// when the station isn't where FL26/FL27 keep it (other game version).
fn contracts_of(mem: &dyn Memory, key: &[u8]) -> Option<Vec<GameContract>> {
    let station = read_u64(mem, mem.base() + CONTRACT_STATION).filter(|&s| s != 0)?;
    if read_vec(mem, station + 8, STATION_NAME.len()).as_deref() != Some(STATION_NAME) {
        return None;
    }
    let list = [0x140, 0x148].iter().find_map(|o| read_u64(mem, station + o).filter(|&p| p != 0)).unwrap_or(station + 0xA0);
    let (begin, end) = (read_u64(mem, list)?, read_u64(mem, list + 8)?);
    let n = end.checked_sub(begin)? / 8;
    if n >= MAX_CONTRACTS {
        return None;
    }
    let handles = read_vec(mem, begin, n as usize * 8)?;
    let vtable = mem.base() + PLAYER_CONTRACT_VTABLE;
    let mut out = Vec::new();
    for h in handles.as_chunks::<8>().0 {
        let ctrl = u64::from_le_bytes(*h);
        let Some(obj) = (ctrl != 0).then(|| read_u64(mem, ctrl + 8)).flatten().filter(|&o| o != 0) else { continue };
        if read_u64(mem, obj) != Some(vtable) {
            continue;
        }
        let (Some(terms), Some(meta)) = (first_ptr(mem, obj, [0xD8, 0xE0, 0xD0]), first_ptr(mem, obj, [0xA8, 0xB0, 0xA0]))
        else {
            continue;
        };
        if read_vec(mem, terms, 8).as_deref() != Some(key) {
            continue;
        }
        let (Some(state), Some(club)) = (read_u32(mem, meta + 4), read_u32(mem, meta + 8)) else { continue };
        out.push(GameContract { terms, meta, state, club });
    }
    Some(out)
}

/// A write: address, new bytes, bytes expected there now.
type Write = (u64, Vec<u8>, Vec<u8>);

fn word(v: u32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

// --- deals: the day-end hook ------------------------------------------------
//
// `stub/live_stub.c` (built into `live_stub.bin`) runs inside the game: the
// app copies it into new executable memory and replaces the first 16 bytes of
// the game's day-end routine with a jump to it. On the game's main thread,
// once per in-game day and before the game's own deal lists, it turns each
// armed request slot into an agreed deal with the game's own routines.

const STUB: &[u8] = include_bytes!("live_stub.bin");
/// Day-end routine of the TransferManager and its first 16 bytes (all
/// position-independent: pushes and `mov rbp, rsp`), replayed by the stub.
const DAY_END: u64 = 0xF1_7160;
const DAY_END_PROLOGUE: [u8; 16] = [
    0x40, 0x55, 0x53, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x48, 0x8B, 0xEC,
];
/// `jmp qword ptr [rip+0]`, followed by the target address.
const JMP_ABS: [u8; 6] = [0xFF, 0x25, 0, 0, 0, 0];
const CAVE_SIZE: usize = 0x2000;
/// Where the blob keeps the ctx pointer and the address to return to (`hook.S`).
const STUB_CTX: usize = 0x80;
const STUB_BACK: usize = 0x88;
/// The ctx block in the cave.
const CTX: u64 = 0x1000;
const MAGIC: &[u8; 8] = b"BOARDRM1";
const CTX_VERSION: u32 = 1;
/// Ctx layout (`Ctx` in `live_stub.c`).
const C_VERSION: u64 = 8;
const C_STATION: u64 = 0x10;
const C_NULL_DATE: u64 = 0x18;
const C_ROUTINES: u64 = 0x20;
const C_SLOTS: u64 = 0x98;
const NSLOTS: u32 = 8;
const SLOT_SIZE: u64 = 0x40;
const S_ERROR: u64 = 0x30;
const SLOT_FREE: u32 = 0;
const SLOT_ARMED: u32 = 1;
const SLOT_DONE: u32 = 2;
const SLOT_FAILED: u32 = 3;
const NULL_DATE: u64 = 0x37F_47F8;
const NO_TEAM: u32 = 0x3FFF;
const TEAM_BITS: u32 = 0x3FFF;
/// Game routines the stub calls, in `Ctx` order, with their first 40 bytes
/// in FL_2027.exe (FL26 has the same code here); any difference refuses.
const ROUTINES: [(u64, &str); 15] = [
    (0x157_65E0, "40534883ec20488bd9e87204f4ff488b50488b82242a64018903488bc34883c4205bc3cccccccccc"), // today
    (0x14B_6A60, "488b05a9f32402c3cccccccccccccccc837930040f94c0c3cccccccccccccccc488b89d8000000e9"), // world
    (0x14B_C9A0, "48895424104883ec288d42034c8bc96683f801763683b99c78830102750ae84d0000004883c428c3"), // find_player
    (0xF2_A2B0, "44894c242044894424184889542410555357488bec4883ec7048c745f0feffffff8b056d0b5f028b"), // build_offer
    (0xF2_A020, "44894c242044894424184889542410555357488bec4883ec7048c745f0feffffff8b05fd0d5f028b"), // make_player_contract
    (0xB1_9690, "40534883ec30488bdac744242000000000488d5178488bcbe8135d3c00488bc34883c4305bc3cccc"), // wrap_handle
    (0xED_F3C0, "4c8b02b8010000004c8901f0410fc100488bc1c3cccccccccccccccccccccccc40534883ec204533"), // copy_handle
    (0xED_F4E0, "40574883ec3048c7442420feffffff48895c2450488bf9488b194885db744983c8fff00fc10383f8"), // free_handle
    (0xED_F970, "48895c2408574883ec20488b9980000000488bf9483b998800000074190f1f00488b0b488b01ff50"), // begin_edit
    (0xED_F760, "40574883ec20488b4178488bf98078100075384883780800743148895c2430488b9980000000483b"), // commit_edit
    (0xF5_8050, "48895424105556574154415541564157488bec4881ec8000000048c745f0feffffff48899c24c000"), // station_add
    (0xF1_2C40, "4885d20f84a30300005556574154415541564157488bec4883ec6048c745f0feffffff48899c24a0"), // deal_list
    (0xEE_E6A0, "48894c2408555357488bec4883ec5048c745e8feffffff488bd9c745d000000000488d4d28e86626"), // negotiations
    (0xED_F9C0, "488b014885c07405488b4008c3c3cccc40565741564883ec4048c7442430feffffff48895c246848"), // get_object
    (0xEF_D220, "488954241048894c2408555356574154415541564157488d6c24e14881ece800000048c7450ffeff"), // charge
];

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn slot_error(code: u32) -> &'static str {
    match code {
        1 => "the game's contract list wasn't there",
        2 => "the player wasn't found in the game",
        3 => "the game refused the deal (clubs or player not valid)",
        4 => "the game refused the new contract",
        _ => "unknown request",
    }
}

enum HookState {
    /// Our stub is in place; its ctx block is at this address.
    Installed(u64),
    /// The game's own code is unchanged and every routine is as expected.
    Ready,
}

fn unsupported() -> Error {
    Error::new(
        "This game version isn't supported for transfers into the running game (its code differs). Save them the \
         usual way (game on the main menu).",
    )
}

fn hook_state(mem: &dyn Memory) -> Result<HookState, Error> {
    let base = mem.base();
    let head = read_vec(mem, base + DAY_END, 16).ok_or_else(unsupported)?;
    if head[..6] == JMP_ABS {
        let cave = u64::from_le_bytes(head[6..14].try_into().unwrap());
        // Ours only if it is this app's stub with this layout.
        let (magic, version) = (read_vec(mem, cave + CTX, 8), read_u32(mem, cave + CTX + C_VERSION));
        return match (magic, version) {
            (Some(m), Some(CTX_VERSION)) if m == MAGIC => Ok(HookState::Installed(cave + CTX)),
            (Some(m), _) if m == MAGIC => Err(Error::new(
                "The game still runs the transfer code of another Boardroom version. Restart the game, then try \
                 again.",
            )),
            _ => Err(Error::new(
                "Another tool already changes the game's day end (for example The Gaffer's live deals). Turn \
                 that off, restart the game, then try again.",
            )),
        };
    }
    if head != DAY_END_PROLOGUE {
        return Err(unsupported());
    }
    for (rva, sig) in ROUTINES {
        let want = unhex(sig);
        if read_vec(mem, base + rva, want.len()).as_deref() != Some(&want[..]) {
            return Err(unsupported());
        }
    }
    Ok(HookState::Ready)
}

/// Put the stub into the game and point the day end at it. -> ctx address.
fn install(mem: &dyn Memory) -> Result<u64, Error> {
    let base = mem.base();
    let cave = mem
        .alloc_code(CAVE_SIZE)
        .ok_or_else(|| Error::new("The game refused memory for the transfer code; nothing was written."))?;
    let mut blob = STUB.to_vec();
    blob[STUB_CTX..STUB_CTX + 8].copy_from_slice(&(cave + CTX).to_le_bytes());
    blob[STUB_BACK..STUB_BACK + 8].copy_from_slice(&(base + DAY_END + 16).to_le_bytes());
    let mut ctx = vec![0u8; (C_SLOTS + NSLOTS as u64 * SLOT_SIZE) as usize];
    ctx[..8].copy_from_slice(MAGIC);
    ctx[8..12].copy_from_slice(&CTX_VERSION.to_le_bytes());
    let mut put = |at: u64, v: u64| ctx[at as usize..at as usize + 8].copy_from_slice(&v.to_le_bytes());
    put(C_STATION, base + CONTRACT_STATION);
    put(C_NULL_DATE, base + NULL_DATE);
    for (k, (rva, _)) in ROUTINES.iter().enumerate() {
        put(C_ROUTINES + 8 * k as u64, base + rva);
    }
    // Code and ctx first; the jump last, so the game never enters a half-written stub.
    let mut jump = JMP_ABS.to_vec();
    jump.extend(cave.to_le_bytes());
    jump.extend([0x90, 0x90]);
    if !mem.write(cave, &blob) || !mem.write(cave + CTX, &ctx) || !mem.write_code(base + DAY_END, &jump) {
        return Err(Error::new("Putting the transfer code into the game failed; nothing was changed in the game."));
    }
    Ok(cave + CTX)
}

/// One request slot as `Slot` in `live_stub.c`; `key` from the game's record.
fn slot_bytes(d: &Deal, key: &[u8]) -> Vec<u8> {
    let mut s = vec![0u8; SLOT_SIZE as usize];
    let mut put = |at: usize, v: u32| s[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(4, d.kind as u32);
    put(0x10, d.buyer);
    put(0x14, d.seller.unwrap_or(NO_TEAM));
    put(0x18, d.fee);
    put(0x1C, d.salary);
    put(0x20, d.end.pack());
    put(0x24, d.release);
    put(0x28, d.loan_end.map_or(0, Date::pack));
    put(0x2C, d.date.map_or(0, Date::pack));
    put(0x34, u32::from(d.user));
    s[8..16].copy_from_slice(key);
    s
}

/// Write the plan into the game behind `mem`. Everything is found and checked
/// first; nothing is written when one change can't be placed.
pub fn apply(plan: &Plan, mem: &dyn Memory) -> Result<Report, Error> {
    if plan.flat.is_empty() && plan.renewals.is_empty() && plan.deals.is_empty() {
        return Err(Error::new("There is nothing to write into the game."));
    }
    let mut guard = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    let found = guard.get_or_insert_with(Found::default);
    let (table, today) = career(mem, found)?;
    if let Some(saved) = plan.game_date
        && saved != today
    {
        return Err(Error::new(format!(
            "The game is on {today}, but the save opened here is from {saved}. Save in the game, open that save here \
             again, then try once more."
        )));
    }

    // Contracts: the player's record must still hold the save's contract.
    let pids: Vec<u32> = plan.renewals.iter().map(|r| r.player_id).chain(plan.deals.iter().map(|d| d.player_id)).collect();
    let records = player_records(mem, table, &pids);
    let record_of = |pid: u32, label: &str| -> Result<(u64, Vec<u8>), Error> {
        let addr = *records
            .get(&pid)
            .ok_or_else(|| Error::new(format!("{label}: player not found in the running game; nothing was written.")))?;
        let rec = read_vec(mem, addr, RECORD as usize)
            .ok_or_else(|| Error::new(format!("{label}: the game's player record can't be read.")))?;
        Ok((addr, rec))
    };
    let mut players = Vec::new();
    for r in &plan.renewals {
        let (addr, rec) = record_of(r.player_id, &r.label)?;
        if u32_at(&rec, R_END) != r.old_end.pack() || u32_at(&rec, R_SALARY) != r.old_salary {
            return Err(Error::new(format!(
                "{}: the game has a different contract than the save opened here (has the game moved on since it \
                 was saved?). Save in the game, open that save here again, then try once more.",
                r.label
            )));
        }
        players.push((addr, rec));
    }
    // Deals: he must still be at the club the save has him at.
    let mut deal_keys = Vec::new();
    for d in &plan.deals {
        let (_, rec) = record_of(d.player_id, &d.label)?;
        if u32_at(&rec, R_CLUB) & TEAM_BITS != d.seller.unwrap_or(NO_TEAM) {
            return Err(Error::new(format!(
                "{}: the game has him at another club than the save opened here (has the game moved on since it \
                 was saved?). Save in the game, open that save here again, then try once more.",
                d.label
            )));
        }
        deal_keys.push(rec[R_KEY..R_KEY + 8].to_vec());
    }
    let hook = if plan.deals.is_empty() { None } else { Some(hook_state(mem)?) };
    // Slots nobody waits for first, then finished ones whose results may still be unread.
    let free_slots: Vec<u32> = match hook {
        Some(HookState::Installed(ctx)) => {
            let states: Vec<(u32, Option<u32>)> =
                (0..NSLOTS).map(|i| (i, read_u32(mem, ctx + C_SLOTS + i as u64 * SLOT_SIZE))).collect();
            let pick = |want: fn(u32) -> bool| states.iter().filter(move |(_, s)| s.is_some_and(want)).map(|(i, _)| *i);
            pick(|s| s == SLOT_FREE).chain(pick(|s| s == SLOT_DONE || s == SLOT_FAILED)).collect()
        }
        _ => (0..NSLOTS).collect(),
    };
    if free_slots.len() < plan.deals.len() {
        return Err(Error::new(format!(
            "{} deals are still waiting for the game's day end; advance a day in the game first.",
            NSLOTS as usize - free_slots.len()
        )));
    }

    // Search patterns for what isn't known from earlier in this run of the game.
    let mut needles: Vec<Vec<u8>> = Vec::new();
    let mut flat_needle = vec![None; plan.flat.len()];
    for (k, c) in plan.flat.iter().enumerate() {
        let known = found.flat.get(&c.offset).filter(|places| places.iter().all(|&a| flat_place_ok(mem, c, a)));
        if known.is_none_or(Vec::is_empty) {
            let (s, n) = c.needle();
            flat_needle[k] = Some((needles.len(), c.context_at + s));
            needles.push(c.context[s..s + n].to_vec());
        }
    }
    let mut terms_needle = vec![None; plan.renewals.len()];
    let mut state_needle = vec![None; plan.renewals.len()];
    for (i, (r, (_, rec))) in plan.renewals.iter().zip(&players).enumerate() {
        let key = &rec[R_KEY..R_KEY + 8];
        if !found.terms.get(&r.player_id).is_some_and(|&a| terms_ok(mem, a, key, r)) {
            terms_needle[i] = Some(needles.len());
            needles.push([key, &r.old_end.pack().to_le_bytes()].concat());
        }
        if r.state_set {
            state_needle[i] = Some(needles.len());
            needles.push(key.to_vec());
        }
    }
    let hits = scan(mem, &needles);

    let mut writes: Vec<Write> = Vec::new();
    let mut flat_copies = 0;
    for (k, c) in plan.flat.iter().enumerate() {
        let places = match flat_needle[k] {
            None => found.flat[&c.offset].clone(),
            Some((n, needle_at)) => {
                let mut places = Vec::new();
                for &hit in &hits[n] {
                    let addr = (c.offset as i128 + hit as i128 - needle_at as i128) as u64;
                    if !places.contains(&addr) && flat_place_ok(mem, c, addr) {
                        places.push(addr);
                    }
                }
                places
            }
        };
        if places.is_empty() {
            return Err(Error::new(
                "The save's data wasn't found in the game's memory as it is in the save opened here (has the game \
                 moved on since it was saved?). Save in the game, open that save here again, then try once more; \
                 nothing was written.",
            ));
        }
        flat_copies += places.len();
        writes.extend(places.iter().map(|&a| (a, c.new.clone(), c.old.clone())));
        found.flat.insert(c.offset, places);
    }

    let mut requests_answered = 0;
    for (i, (r, (record, rec))) in plan.renewals.iter().zip(&players).enumerate() {
        let key = &rec[R_KEY..R_KEY + 8];
        let terms = match terms_needle[i] {
            None => found.terms[&r.player_id],
            Some(n) => {
                let terms: Vec<u64> = hits[n].iter().copied().filter(|&a| terms_ok(mem, a, key, r)).collect();
                let [terms] = terms[..] else {
                    return Err(Error::new(format!(
                        "{}: the contract wasn't found in the game's memory ({} matches); nothing was written.",
                        r.label,
                        terms.len()
                    )));
                };
                found.terms.insert(r.player_id, terms);
                terms
            }
        };
        let club = u32_at(rec, R_CLUB);
        let station = contracts_of(mem, key);
        let request = station.as_ref().and_then(|cs| {
            cs.iter().find(|c| c.club == club && REQUEST_STATES.contains(&c.state) && c.terms != terms)
        });
        let mut new_terms = word(r.end.pack());
        new_terms.extend(word(r.salary));
        match request {
            // As "Accept Terms" in the game: the request carries the new
            // contract and is accepted today; the current one is replaced.
            Some(req) => {
                let main = station.as_ref().unwrap().iter().find(|c| c.terms == terms && c.state == STATE_CONTRACTED);
                let Some(main) = main else {
                    return Err(Error::new(format!(
                        "{}: the game's current contract wasn't found next to its renewal request; nothing was written.",
                        r.label
                    )));
                };
                let now = |a: u64, n: usize| read_vec(mem, a, n).unwrap_or_default();
                writes.push((req.terms + T_END, new_terms, now(req.terms + T_END, 8)));
                writes.push((req.terms + T_RELEASE, word(r.release), now(req.terms + T_RELEASE, 4)));
                writes.push((req.meta + 0xC, word(today.pack()), now(req.meta + 0xC, 4)));
                writes.push((req.meta + 4, word(STATE_ACCEPTED), word(req.state)));
                writes.push((main.meta + 4, word(STATE_REPLACED), word(STATE_CONTRACTED)));
                requests_answered += 1;
            }
            None if r.request_open => {
                return Err(Error::new(format!(
                    "{}: the save has a renewal request open, but the running game {} it; nothing was written.",
                    r.label,
                    if station.is_some() { "no longer has" } else { "doesn't show" }
                )));
            }
            None => {
                let mut old_terms = word(r.old_end.pack());
                old_terms.extend(word(r.old_salary));
                writes.push((terms + T_END, new_terms, old_terms));
                if r.release != r.old_release {
                    let now = read_vec(mem, terms + T_RELEASE, 4).unwrap_or_default();
                    writes.push((terms + T_RELEASE, word(r.release), now));
                }
            }
        }
        writes.push((record + R_END as u64, word(r.end.pack()), word(r.old_end.pack())));
        writes.push((record + R_SALARY as u64, word(r.salary), word(r.old_salary)));
        if r.release != r.old_release {
            let flags = u32_at(rec, R_FLAGS);
            let set = if r.release > 0 { flags | HAS_RELEASE_CLAUSE } else { flags & !HAS_RELEASE_CLAUSE };
            writes.push((record + R_FLAGS as u64, word(set), word(flags)));
        }
        // Renewal state rows `(key, state, x)`, after a row of the same kind;
        // only x's high byte may be set.
        if let Some(n) = state_needle[i] {
            let lo = u32_at(key, 0);
            for &a in &hits[n] {
                let Some(row) = a.checked_sub(16).and_then(|s| read_vec(mem, s, 32)) else { continue };
                let (prev, state, zero) = (u32_at(&row, 0), u32_at(&row, 24), u32_at(&row, 28));
                if (1..=3).contains(&state)
                    && zero & crate::masterleague::STATE_ROW_ZERO_MASK == 0
                    && prev >> 16 == lo >> 16
                {
                    writes.push((a + 8, word(0), word(state)));
                }
            }
        }
    }

    // Last check right before writing: the game may have run on meanwhile.
    if writes.iter().any(|(addr, _, old)| read_vec(mem, *addr, old.len()).as_deref() != Some(&old[..])) {
        return Err(Error::new(
            "The game changed while the changes were being placed (did a day pass?). Nothing was written; try again.",
        ));
    }
    // The day-end hook changes nothing by itself: put it in first.
    let ctx = match hook {
        Some(HookState::Installed(ctx)) => Some(ctx),
        Some(HookState::Ready) => Some(install(mem)?),
        None => None,
    };
    for (done, (addr, new, _)) in writes.iter().enumerate() {
        if !mem.write(*addr, new) || read_vec(mem, *addr, new.len()).as_deref() != Some(&new[..]) {
            return Err(Error::new(format!(
                "Writing into the game failed after {done} of {} changes. Don't save in the game: load your last \
                 save there.",
                writes.len()
            )));
        }
    }
    // Each slot: everything but its state, then the state that arms it.
    let mut deal_slots = Vec::new();
    if let Some(ctx) = ctx {
        for ((d, key), &i) in plan.deals.iter().zip(&deal_keys).zip(&free_slots) {
            let at = ctx + C_SLOTS + i as u64 * SLOT_SIZE;
            let slot = slot_bytes(d, key);
            if !mem.write(at + 4, &slot[4..]) || !mem.write(at, &word(SLOT_ARMED)) {
                return Err(Error::new(format!(
                    "{}: handing the deal to the game failed. Don't save in the game: load your last save there.",
                    d.label
                )));
            }
            deal_slots.push(i);
        }
    }
    Ok(Report { flat_copies, renewals: plan.renewals.len(), requests_answered, deal_slots })
}

#[cfg(not(windows))]
fn open_game(_extra_exe: &[String]) -> Result<Box<dyn Memory>, Error> {
    Err(Error::new("Writing into the running game works on Windows only."))
}

#[cfg(windows)]
fn open_game(extra_exe: &[String]) -> Result<Box<dyn Memory>, Error> {
    let mut names: Vec<String> = EXE_NAMES.iter().map(|n| n.to_string()).collect();
    for n in extra_exe {
        if !names.iter().any(|k| k.eq_ignore_ascii_case(n)) {
            names.push(n.clone());
        }
    }
    let candidates = windows::running(&names);
    if candidates.is_empty() {
        return Err(Error::new(format!("The game isn't running (looked for {}).", names.join(", "))));
    }
    let mut denied = false;
    for pid in candidates {
        match windows::Process::open(pid) {
            // Tools next to the game (Sider, launchers) don't have the Master League pointer.
            Some(p) if read_u64(&p, p.base() + OFF_OBJ).is_some() => return Ok(Box::new(p)),
            Some(_) => {}
            None => denied = true,
        }
    }
    Err(Error::new(if denied {
        "Couldn't open the game for writing. If the game runs as administrator, start Boardroom as administrator too."
    } else {
        "The running game isn't a version this app can write into."
    }))
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::Debug::{FlushInstructionCache, ReadProcessMemory, WriteProcessMemory};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPMODULE, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_PRIVATE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_GUARD,
        PAGE_READWRITE, VirtualAllocEx, VirtualProtectEx, VirtualQueryEx,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
    };

    use super::Memory;

    /// The game's address space ends below this.
    const USER_END: u64 = 0x7FFF_FFFF_0000;

    pub struct Process {
        handle: HANDLE,
        base: u64,
        pid: u32,
    }

    fn wide_eq(name: &[u16], want: &str) -> bool {
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case(want)
    }

    /// Running processes with one of `names`, in the order of `names`.
    pub fn running(names: &[String]) -> Vec<u32> {
        let mut found: Vec<(usize, u32)> = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return Vec::new();
            }
            let mut entry: PROCESSENTRY32W = zeroed();
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut entry) != 0;
            while ok {
                if let Some(rank) = names.iter().position(|n| wide_eq(&entry.szExeFile, n)) {
                    found.push((rank, entry.th32ProcessID));
                }
                ok = Process32NextW(snap, &mut entry) != 0;
            }
            CloseHandle(snap);
        }
        found.sort_unstable();
        found.into_iter().map(|(_, pid)| pid).collect()
    }

    /// Load address of the process's executable (its first module).
    fn exe_base(pid: u32) -> Option<u64> {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, pid);
            if snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut entry: MODULEENTRY32W = zeroed();
            entry.dwSize = size_of::<MODULEENTRY32W>() as u32;
            let base = (Module32FirstW(snap, &mut entry) != 0).then_some(entry.modBaseAddr as u64);
            CloseHandle(snap);
            base
        }
    }

    impl Process {
        /// `None` when Windows refuses access (for example a game run as administrator).
        pub fn open(pid: u32) -> Option<Process> {
            let access = PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_INFORMATION;
            let handle = unsafe { OpenProcess(access, 0, pid) };
            if handle.is_null() {
                return None;
            }
            // The game's exe isn't relocated; fall back to its fixed address.
            let base = exe_base(pid).unwrap_or(0x1_4000_0000);
            Some(Process { handle, base, pid })
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.handle) };
        }
    }

    impl Memory for Process {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            let mut got = 0usize;
            let ok = unsafe {
                ReadProcessMemory(self.handle, addr as *const c_void, buf.as_mut_ptr().cast(), buf.len(), &mut got)
            };
            ok != 0 && got == buf.len()
        }

        fn write(&self, addr: u64, data: &[u8]) -> bool {
            let mut put = 0usize;
            let ok = unsafe {
                WriteProcessMemory(self.handle, addr as *const c_void, data.as_ptr().cast(), data.len(), &mut put)
            };
            ok != 0 && put == data.len()
        }

        fn regions(&self) -> Vec<(u64, u64)> {
            let mut out = Vec::new();
            let mut at = 0x1_0000u64;
            while at < USER_END {
                let mut info: MEMORY_BASIC_INFORMATION = unsafe { zeroed() };
                let n = unsafe {
                    VirtualQueryEx(self.handle, at as *const c_void, &mut info, size_of::<MEMORY_BASIC_INFORMATION>())
                };
                if n == 0 || info.RegionSize == 0 {
                    break;
                }
                let (start, size) = (info.BaseAddress as u64, info.RegionSize as u64);
                let writable = info.Protect & (PAGE_READWRITE | PAGE_EXECUTE_READWRITE) != 0;
                if info.State == MEM_COMMIT && info.Type == MEM_PRIVATE && writable && info.Protect & PAGE_GUARD == 0 {
                    out.push((start, size));
                }
                at = start + size;
            }
            out
        }

        fn base(&self) -> u64 {
            self.base
        }

        fn process_id(&self) -> u32 {
            self.pid
        }

        fn alloc_code(&self, size: usize) -> Option<u64> {
            let p = unsafe {
                VirtualAllocEx(self.handle, std::ptr::null(), size, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE)
            };
            (!p.is_null()).then_some(p as u64)
        }

        fn write_code(&self, addr: u64, data: &[u8]) -> bool {
            let target = addr as *const c_void;
            let mut old = 0u32;
            if unsafe { VirtualProtectEx(self.handle, target, data.len(), PAGE_EXECUTE_READWRITE, &mut old) } == 0 {
                return false;
            }
            let ok = self.write(addr, data);
            let mut ignored = 0u32;
            unsafe {
                VirtualProtectEx(self.handle, target, data.len(), old, &mut ignored);
                FlushInstructionCache(self.handle, target, data.len());
            }
            ok
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearby_changes_form_one_range_with_context() {
        let old = vec![0u8; 10_000];
        let mut new = old.clone();
        new[5000] = 1;
        new[5020] = 2;
        new[9000] = 3;
        let changes = FlatChange::group(&old, &new, &[5020, 5000, 9000]);
        assert_eq!(changes.len(), 2);
        assert_eq!((changes[0].offset, changes[0].new.len()), (5000, 21));
        assert_eq!(changes[0].context_at, 5000 - CONTEXT);
        assert_eq!(changes[1].context_at + changes[1].context.len(), old.len());
    }

    #[test]
    fn needle_widens_until_it_holds_enough_values() {
        let mut old = vec![0u8; 8192];
        for (k, o) in (0..NEEDLE_NONZERO).map(|k| (k, 4096 + 100 + 8 * k)) {
            old[o] = k as u8 + 1;
        }
        let mut new = old.clone();
        new[4096] = 9;
        let c = &FlatChange::group(&old, &new, &[4096])[0];
        let (s, n) = c.needle();
        let window = &c.context[s..s + n];
        assert_eq!(window.iter().filter(|&&x| x != 0).count(), NEEDLE_NONZERO);
        assert!(c.context_at + s <= c.offset && c.offset < c.context_at + s + n);
    }

    /// A game with one player record (player 777 at club 12), the career
    /// date, and the code the stub relies on.
    struct Game {
        pieces: std::cell::RefCell<Vec<(u64, Vec<u8>)>>,
    }

    const BASE: u64 = 0x1_4000_0000;
    const TABLE: u64 = 0x3_0000_0000;
    const KEY: [u8; 8] = [0x31, 0, 0, 0, 0x09, 0x03, 0, 0];

    impl Memory for Game {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            let pieces = self.pieces.borrow();
            let Some((at, b)) = pieces.iter().find(|(a, b)| *a <= addr && addr + buf.len() as u64 <= a + b.len() as u64)
            else {
                return false;
            };
            let o = (addr - at) as usize;
            buf.copy_from_slice(&b[o..o + buf.len()]);
            true
        }
        fn write(&self, addr: u64, data: &[u8]) -> bool {
            let mut pieces = self.pieces.borrow_mut();
            let Some((at, b)) =
                pieces.iter_mut().find(|(a, b)| *a <= addr && addr + data.len() as u64 <= *a + b.len() as u64)
            else {
                return false;
            };
            let o = (addr - *at) as usize;
            b[o..o + data.len()].copy_from_slice(data);
            true
        }
        fn regions(&self) -> Vec<(u64, u64)> {
            Vec::new()
        }
        fn base(&self) -> u64 {
            BASE
        }
        fn process_id(&self) -> u32 {
            7
        }
        fn alloc_code(&self, size: usize) -> Option<u64> {
            let at = 0x7_0000_0000 + 0x1_0000 * self.pieces.borrow().len() as u64;
            self.pieces.borrow_mut().push((at, vec![0; size]));
            Some(at)
        }
        fn write_code(&self, addr: u64, data: &[u8]) -> bool {
            self.write(addr, data)
        }
    }

    fn game() -> Game {
        let mut record = vec![0u8; RECORD as usize];
        record[R_KEY..R_KEY + 8].copy_from_slice(&KEY);
        record[R_PLAYER_AGAIN..R_PLAYER_AGAIN + 4].copy_from_slice(&777u32.to_le_bytes());
        record[R_CLUB..R_CLUB + 4].copy_from_slice(&(0xFFFF_C000u32 | 12).to_le_bytes());
        let mut obj = vec![0u8; 0x50];
        obj[0x48..].copy_from_slice(&TABLE.to_le_bytes());
        let mut day_end = DAY_END_PROLOGUE.to_vec();
        day_end.extend([0x48, 0x83, 0xEC, 0x58]);
        let mut pieces = vec![
            (BASE + OFF_OBJ, 0x2_0000_0000u64.to_le_bytes().to_vec()),
            (0x2_0000_0000, obj),
            (TABLE, record),
            (TABLE + OFF_DATE, Date::new(2025, 8, 6).unwrap().pack().to_le_bytes().to_vec()),
            (BASE + DAY_END, day_end),
        ];
        pieces.extend(ROUTINES.iter().map(|(rva, sig)| (BASE + rva, unhex(sig))));
        Game { pieces: std::cell::RefCell::new(pieces) }
    }

    fn sale() -> Plan {
        Plan {
            game_date: Date::new(2025, 8, 6),
            deals: vec![Deal {
                label: "Sold".into(),
                kind: DealKind::Transfer,
                player_id: 777,
                buyer: 40,
                seller: Some(12),
                fee: 125_000,
                salary: 30_000,
                end: Date::new(2029, 6, 30).unwrap(),
                release: 0,
                loan_end: None,
                date: None,
                user: true,
            }],
            ..Plan::default()
        }
    }

    fn u64_at(g: &Game, addr: u64) -> u64 {
        read_u64(g, addr).unwrap()
    }

    #[test]
    fn a_deal_puts_the_stub_in_and_arms_a_request() {
        let g = game();
        let report = apply(&sale(), &g).unwrap();
        assert_eq!(report.deal_slots, vec![0]);

        // The day end jumps to the stub, which returns behind the moved prologue.
        let head = read_vec(&g, BASE + DAY_END, 16).unwrap();
        assert_eq!(head[..6], JMP_ABS);
        let cave = u64::from_le_bytes(head[6..14].try_into().unwrap());
        let code = read_vec(&g, cave, STUB.len()).unwrap();
        assert_eq!(code[..STUB_CTX], STUB[..STUB_CTX]);
        assert_eq!(u64_at(&g, cave + STUB_CTX as u64), cave + CTX);
        assert_eq!(u64_at(&g, cave + STUB_BACK as u64), BASE + DAY_END + 16);

        let ctx = cave + CTX;
        assert_eq!(read_vec(&g, ctx, 8).unwrap(), MAGIC);
        assert_eq!(u64_at(&g, ctx + C_STATION), BASE + CONTRACT_STATION);
        for (k, (rva, _)) in ROUTINES.iter().enumerate() {
            assert_eq!(u64_at(&g, ctx + C_ROUTINES + 8 * k as u64), BASE + rva);
        }
        let slot = read_vec(&g, ctx + C_SLOTS, SLOT_SIZE as usize).unwrap();
        let w = |o: usize| u32_at(&slot, o);
        assert_eq!((w(0), w(4)), (SLOT_ARMED, DealKind::Transfer as u32));
        assert_eq!(slot[8..16], KEY, "the game's own key for the player");
        assert_eq!((w(0x10), w(0x14), w(0x18), w(0x1C)), (40, 12, 125_000, 30_000));
        assert_eq!((w(0x20), w(0x2C), w(0x34)), (Date::new(2029, 6, 30).unwrap().pack(), 0, 1));

        // A second deal reuses the stub and takes the next slot.
        let report = apply(&sale(), &g).unwrap();
        assert_eq!(report.deal_slots, vec![1]);
        assert_eq!(read_vec(&g, BASE + DAY_END, 16).unwrap(), head);
    }

    #[test]
    fn other_code_at_the_day_end_is_left_alone() {
        // Another tool's jump there.
        let g = game();
        g.write(BASE + DAY_END, &[JMP_ABS.as_slice(), &0x9_0000_0000u64.to_le_bytes(), &[0x90, 0x90]].concat());
        g.pieces.borrow_mut().push((0x9_0000_0000 + CTX, vec![0; 8]));
        let before = g.pieces.borrow().clone();
        assert!(apply(&sale(), &g).is_err());
        assert_eq!(*g.pieces.borrow(), before);

        // A different game version.
        let g = game();
        let (rva, _) = ROUTINES[3];
        g.write(BASE + rva + 30, &[0]);
        let before = g.pieces.borrow().clone();
        assert!(apply(&sale(), &g).is_err());
        assert_eq!(*g.pieces.borrow(), before);
    }

    #[test]
    fn a_player_the_game_has_elsewhere_is_refused() {
        let g = game();
        let mut plan = sale();
        plan.deals[0].seller = Some(13);
        assert!(apply(&plan, &g).is_err());
        assert_eq!(read_vec(&g, BASE + DAY_END, 16).unwrap(), DAY_END_PROLOGUE, "nothing installed");
    }
}
