//! Writing changes into a running game, against a fake game memory laid out
//! like Football Life's.

mod common;

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};

use common::*;
use boardroom::live::{self, Memory};
use boardroom::masterleague::{self as ml, LoanRequest, MasterLeague, Source, TransferRequest};

const BASE: u64 = 0x1_4000_0000;
const OBJ: u64 = 0x2_0000_0000;
const TABLE: u64 = 0x3_0000_0000;
const SAVE_COPY: u64 = 0x4_0000_1230;
const HEAP: u64 = 0x5_0000_0000;
const STATION: u64 = 0x6_0000_0000;
const RECORD: u64 = 0x17C;
const OFF_DATE: u64 = 0x164_2A24;
/// Part of the save the fake game keeps byte for byte: coach table to finance block.
const COPIED: std::ops::Range<usize> = ml::COACH_TABLE..ml::FL26_FINANCE + 0x4000;
/// The player's club as the game's record holds it.
const GAME_CLUB: u32 = 0x1234;
const RENEWED: usize = 2;

static RUN: AtomicU32 = AtomicU32::new(1);

struct Game {
    pieces: RefCell<Vec<(u64, Vec<u8>)>>,
    run: u32,
}

impl Memory for Game {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let pieces = self.pieces.borrow();
        let Some((at, bytes)) = pieces.iter().find(|(a, b)| *a <= addr && addr + buf.len() as u64 <= a + b.len() as u64)
        else {
            return false;
        };
        let o = (addr - at) as usize;
        buf.copy_from_slice(&bytes[o..o + buf.len()]);
        true
    }

    fn write(&self, addr: u64, data: &[u8]) -> bool {
        let mut pieces = self.pieces.borrow_mut();
        let Some((at, bytes)) =
            pieces.iter_mut().find(|(a, b)| *a <= addr && addr + data.len() as u64 <= *a + b.len() as u64)
        else {
            return false;
        };
        let o = (addr - *at) as usize;
        bytes[o..o + data.len()].copy_from_slice(data);
        true
    }

    fn regions(&self) -> Vec<(u64, u64)> {
        self.pieces.borrow().iter().map(|(a, b)| (*a, b.len() as u64)).collect()
    }

    fn base(&self) -> u64 {
        BASE
    }

    fn process_id(&self) -> u32 {
        self.run
    }

    fn alloc_code(&self, _size: usize) -> Option<u64> {
        None
    }

    fn write_code(&self, _addr: u64, _data: &[u8]) -> bool {
        false
    }
}

impl Game {
    fn u32(&self, addr: u64) -> u32 {
        let mut b = [0; 4];
        assert!(self.read(addr, &mut b));
        u32::from_le_bytes(b)
    }

    fn put(&self, addr: u64, bytes: &[u8]) {
        assert!(self.write(addr, bytes));
    }

    fn add(&self, addr: u64, bytes: Vec<u8>) {
        self.pieces.borrow_mut().push((addr, bytes));
    }

    fn snapshot(&self) -> Vec<(u64, Vec<u8>)> {
        self.pieces.borrow().clone()
    }

    fn bytes(&self, addr: u64, n: usize) -> Vec<u8> {
        let mut b = vec![0; n];
        assert!(self.read(addr, &mut b));
        b
    }
}

fn saved(off: usize) -> u64 {
    SAVE_COPY + (off - COPIED.start) as u64
}
fn record_at(j: usize) -> u64 {
    TABLE + j as u64 * RECORD
}
fn terms_at(j: usize) -> u64 {
    HEAP + 0x100 + j as u64 * 0x40
}
fn key(j: usize) -> Vec<u8> {
    let p = club_player(MANAGED, j);
    [p.internal_id.to_le_bytes(), p.player_id.to_le_bytes()].concat()
}
fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// The game with the save loaded: the managed squad's records and contract
/// terms, the in-game date and a byte-for-byte copy of part of the save.
fn running_game(save: &[u8]) -> Game {
    let mut obj = vec![0u8; 0x100];
    obj[0x48..0x50].copy_from_slice(&TABLE.to_le_bytes());
    let mut records = vec![0u8; SQUAD * RECORD as usize];
    let mut heap = vec![0u8; 0x2000];
    for j in 0..SQUAD {
        let p = club_player(MANAGED, j);
        let (salary, end) = managed_terms(j);
        let r = j * RECORD as usize;
        records[r + 0x2C..r + 0x34].copy_from_slice(&key(j));
        records[r + 0x34..r + 0x38].copy_from_slice(&p.player_id.to_le_bytes());
        records[r + 0x12C..r + 0x130].copy_from_slice(&GAME_CLUB.to_le_bytes());
        records[r + 0x138..r + 0x13C].copy_from_slice(&end.pack().to_le_bytes());
        records[r + 0x144..r + 0x148].copy_from_slice(&0x0000_0101u32.to_le_bytes());
        records[r + 0x15C..r + 0x160].copy_from_slice(&salary.to_le_bytes());
        let t = (terms_at(j) - HEAP) as usize;
        heap[t..t + 8].copy_from_slice(&key(j));
        heap[t + 8..t + 20].copy_from_slice(&words(&[end.pack(), salary, 0]));
    }
    let (y, m, d) = GAME_DATE;
    Game {
        pieces: RefCell::new(vec![
            (BASE + 0x370_5E10, OBJ.to_le_bytes().to_vec()),
            (OBJ, obj),
            (TABLE, records),
            (TABLE + OFF_DATE, [y.to_le_bytes().as_slice(), &[m, d]].concat()),
            (SAVE_COPY, save[COPIED].to_vec()),
            (HEAP, heap),
        ]),
        run: RUN.fetch_add(1, Ordering::Relaxed),
    }
}

fn open_data(dir: &TempDir, data: Vec<u8>) -> MasterLeague {
    let path = write_save(&dir.0.join("ML00000001"), data);
    let mut game = MasterLeague::open(&path).unwrap();
    game.set_managed_team(MANAGED).unwrap();
    game
}

fn open(dir: &TempDir) -> MasterLeague {
    open_data(dir, build_data(false))
}

/// Budget change and one renewal, with the game as it was when the save was opened.
fn edited(dir: &TempDir) -> (MasterLeague, Game) {
    let mut save = open(dir);
    let game = running_game(&save.data);
    save.set_finances(80_000_000, 30_000_000).unwrap();
    save.renew_contract(club_player(MANAGED, RENEWED), 2_500_000, date(2030, 6, 30), None, Some("Renewed")).unwrap();
    (save, game)
}

#[test]
fn budget_and_renewal_reach_the_running_game() {
    let dir = TempDir::new("live-ok");
    let (mut save, game) = edited(&dir);
    let (plan, count) = save.live_plan().unwrap();
    let report = live::apply(&plan, &game).unwrap();
    assert_eq!(report, live::Report { flat_copies: 1, renewals: 1, requests_answered: 0, deal_slots: Vec::new() });

    let finance = saved(ml::FL26_FINANCE);
    assert_eq!(game.u32(finance), 800_000);
    assert_eq!(game.u32(finance + ml::SALARY_BUDGET_OFFSET as u64), 300_000);
    let end = date(2030, 6, 30).pack();
    assert_eq!((game.u32(terms_at(RENEWED) + 8), game.u32(terms_at(RENEWED) + 12)), (end, 25_000));
    assert_eq!((game.u32(record_at(RENEWED) + 0x138), game.u32(record_at(RENEWED) + 0x15C)), (end, 25_000));
    let (salary, old_end) = managed_terms(RENEWED + 1);
    assert_eq!((game.u32(terms_at(RENEWED + 1) + 8), game.u32(terms_at(RENEWED + 1) + 12)), (old_end.pack(), salary));

    // In the game now: no undo here, and nothing new to write.
    save.mark_written_to_game(count);
    assert!(save.undo().is_err());
    assert!(save.live_plan().is_err());
    save.set_finances(90_000_000, 30_000_000).unwrap();
    let (plan, _) = save.live_plan().unwrap();
    assert!(plan.renewals.is_empty());
    live::apply(&plan, &game).unwrap();
    assert_eq!(game.u32(finance), 900_000);
}

#[test]
fn game_that_moved_on_is_left_alone() {
    let dir = TempDir::new("live-moved");
    let (save, game) = edited(&dir);
    let (plan, _) = save.live_plan().unwrap();

    // The game spent money since the save was written.
    game.put(saved(ml::FL26_FINANCE), &499_000u32.to_le_bytes());
    let before = game.snapshot();
    assert!(live::apply(&plan, &game).is_err());
    assert_eq!(game.snapshot(), before, "nothing may be written");

    // A day has passed.
    game.put(saved(ml::FL26_FINANCE), &500_000u32.to_le_bytes());
    let (y, m, d) = GAME_DATE;
    game.put(TABLE + OFF_DATE, &[y.to_le_bytes().as_slice(), &[m, d + 1]].concat());
    let before = game.snapshot();
    assert!(live::apply(&plan, &game).is_err());
    assert_eq!(game.snapshot(), before, "nothing may be written");
}

#[test]
fn transfers_and_loans_go_to_the_game_as_deals() {
    let dir = TempDir::new("live-transfer");
    let mut save = open(&dir);
    let sold = club_player(MANAGED, 0);
    let mut sale = TransferRequest::new(Source::Club(MANAGED), 3, sold.player_id);
    sale.fee_eur = 1_000_000;
    save.transfer(sale).unwrap();
    let signed = club_player(7, 2);
    save.transfer(TransferRequest {
        fee_eur: 12_000_000,
        salary_eur: Some(2_500_000),
        contract_end: Some(date(2029, 6, 30)),
        release_clause_eur: Some(40_000_000),
        ..TransferRequest::new(Source::Club(7), MANAGED, signed.player_id)
    })
    .unwrap();
    let loaned = club_player(9, 1);
    save.loan(LoanRequest {
        club: 9,
        outgoing: false,
        player_id: loaned.player_id,
        fee_eur: 500_000,
        loan_end: date(2026, 6, 30),
        use_transfer_budget: true,
        wait_for_window: true,
        name: None,
    })
    .unwrap();

    let (plan, _) = save.live_plan().unwrap();
    assert!(plan.flat.is_empty(), "the game charges the fees itself; the save's budget bytes are not written");
    let [sale, signing, loan] = &plan.deals[..] else { panic!("three deals: {:?}", plan.deals) };
    let (salary, end) = managed_terms(0);
    assert_eq!((sale.kind, sale.buyer, sale.seller, sale.fee), (live::DealKind::Transfer, 3, Some(MANAGED as u32), 10_000));
    assert_eq!((sale.salary, sale.end, sale.user), (salary, end, true), "he keeps his terms at the buying club");
    assert_eq!((signing.buyer, signing.seller, signing.fee), (MANAGED as u32, Some(7), 120_000));
    assert_eq!((signing.salary, signing.end, signing.release), (25_000, date(2029, 6, 30), 400_000));
    assert_eq!(signing.date, None, "the window is open on 6 Aug: next day end");
    assert_eq!((loan.kind, loan.buyer, loan.seller, loan.fee), (live::DealKind::Loan, MANAGED as u32, Some(9), 5_000));
    assert_eq!(loan.loan_end, Some(date(2026, 6, 30)));

    // A budget change with deals the game charges would count the fees twice.
    save.set_finances(90_000_000, 30_000_000).unwrap();
    assert!(save.live_plan().is_err());
}

#[test]
fn releases_are_not_written_into_the_running_game() {
    let dir = TempDir::new("live-release");
    let mut save = open(&dir);
    save.release(club_player(MANAGED, 0), 0, false, None).unwrap();
    assert!(save.live_plan().is_err());
}

#[test]
fn release_clause_and_manager_name_reach_the_running_game() {
    let dir = TempDir::new("live-clause");
    let mut save = open(&dir);
    let game = running_game(&save.data);
    let (salary, end) = managed_terms(RENEWED);
    let player = club_player(MANAGED, RENEWED);
    save.renew_contract(player, salary as i64 * 100, end, Some(40_000_000), None).unwrap();
    assert_eq!(save.contract_of(&player).unwrap().release_units, 400_000);
    save.set_manager_name("New Boss").unwrap();
    let (plan, count) = save.live_plan().unwrap();
    live::apply(&plan, &game).unwrap();
    save.mark_written_to_game(count);

    assert_eq!(game.u32(terms_at(RENEWED) + 0x10), 400_000);
    assert_eq!(game.u32(record_at(RENEWED) + 0x144), 0x4000_0101, "has a release clause");
    for at in manager_names() {
        assert_eq!(&game.bytes(saved(at), 9), b"New Boss\0");
    }

    // Removing it again.
    save.renew_contract(player, salary as i64 * 100, end, Some(0), None).unwrap();
    let (plan, _) = save.live_plan().unwrap();
    live::apply(&plan, &game).unwrap();
    assert_eq!(game.u32(terms_at(RENEWED) + 0x10), 0);
    assert_eq!(game.u32(record_at(RENEWED) + 0x144), 0x0000_0101);
}

/// The save with a renewal request of the managed club open for `j` (a
/// negotiation row) and his renewal state row set to `state`.
const STATE_ROW_X: u32 = 0xC400_0000;

fn with_request(j: usize, state: u32) -> Vec<u8> {
    let mut data = build_data(false);
    let p = club_player(MANAGED, j);
    let (salary, end) = managed_terms(j);
    let req = row(p, salary + 500, 0, date(end.year + 2, 6, 30), NEGOTIATION_INDEX + 1, team_ref(MANAGED), 1, None);
    put_words(&mut data, FIRST_ROW + (SQUAD + 1) * boardroom::contracts::RECORD, &req);
    // State rows behind the player records, ~15 KB before the end.
    let mut extra = vec![0u8; 0x4000];
    let at = 0x500;
    // The last word's high byte varies per player (FL27); the rest is zero.
    extra[at - 16..at - 8].copy_from_slice(&key(j - 1));
    extra[at - 4..at].copy_from_slice(&0xD600_0000u32.to_le_bytes());
    extra[at..at + 8].copy_from_slice(&key(j));
    extra[at + 8..at + 12].copy_from_slice(&state.to_le_bytes());
    extra[at + 12..at + 16].copy_from_slice(&STATE_ROW_X.to_le_bytes());
    let tail_len = TAIL.len() + extra.len();
    data.extend(extra);
    put_words(&mut data, PLAYERS_AT - 12, &[tail_len as u32]);
    data
}

#[test]
fn renewal_answers_the_games_open_request() {
    let dir = TempDir::new("live-request");
    let j = 5;
    let mut save = open_data(&dir, with_request(j, 1));
    let game = running_game(&save.data);
    // The game's ContractStation: his contract (state 5) and the request (state 2).
    let req_terms = HEAP + 0x1800;
    game.put(req_terms, &[key(j), words(&[date(2031, 6, 30).pack(), 99, 7])].concat());
    let (main_meta, req_meta) = (HEAP + 0x1900, HEAP + 0x1940);
    game.put(main_meta, &words(&[9, 5, GAME_CLUB, 0]));
    game.put(req_meta, &words(&[10, 2, GAME_CLUB, 0]));
    let mut station = vec![0u8; 0x800];
    let mut put = |o: usize, v: u64| station[o..o + 8].copy_from_slice(&v.to_le_bytes());
    // Name check, then the list: its header at +0x200 (begin, end) and two handles.
    put(0x140, STATION + 0x200);
    put(0x200, STATION + 0x210);
    put(0x208, STATION + 0x220);
    let objects = [(STATION + 0x300, terms_at(j), main_meta), (STATION + 0x500, req_terms, req_meta)];
    for (k, (obj, terms, meta)) in objects.into_iter().enumerate() {
        let handle = 0x280 + 0x10 * k;
        put(0x210 + 8 * k, STATION + handle as u64);
        put(handle + 8, obj);
        let o = (obj - STATION) as usize;
        put(o, BASE + 0x27F_78A0);
        put(o + 0xA8, meta);
        put(o + 0xD8, terms);
    }
    station[8..23].copy_from_slice(b"ContractStation");
    game.add(STATION, station);
    game.add(BASE + 0x36F_CD78, STATION.to_le_bytes().to_vec());
    // Two copies of his renewal state row ("asked for a renewal").
    let state_rows = [HEAP + 0x1A00, HEAP + 0x1B00];
    for at in state_rows {
        game.put(at - 16, &key(j - 1));
        game.put(at, &[key(j), words(&[1, 0])].concat());
    }

    let player = club_player(MANAGED, j);
    let new_end = date(2031, 6, 30);
    let op = save.renew_contract(player, 3_000_000, new_end, None, None).unwrap();
    assert!(op.description.contains("running game only"));
    assert!(save.build().is_err(), "the file can't take an answer to the game's request");
    let state_row = save.data.len() - 0x4000 + 0x500 + 8;
    assert_eq!(u32_at(&save.data, state_row), 0, "no longer asked for a renewal");

    let (plan, _) = save.live_plan().unwrap();
    let report = live::apply(&plan, &game).unwrap();
    assert_eq!(report.requests_answered, 1);
    let today = date(GAME_DATE.0, GAME_DATE.1, GAME_DATE.2).pack();
    // As "Accept Terms" in the game: the request carries the contract (with
    // the clause the app shows), accepted today.
    let clause = save.contract_of(&player).unwrap().release_units;
    assert_eq!(game.bytes(req_terms + 8, 12), words(&[new_end.pack(), 30_000, clause]));
    assert_eq!(game.bytes(req_meta + 4, 12), words(&[4, GAME_CLUB, today]));
    assert_eq!(game.u32(main_meta + 4), 7, "current contract replaced");
    let (salary, end) = managed_terms(j);
    assert_eq!(game.bytes(terms_at(j) + 8, 8), words(&[end.pack(), salary]), "current terms untouched");
    assert_eq!((game.u32(record_at(j) + 0x138), game.u32(record_at(j) + 0x15C)), (new_end.pack(), 30_000));
    for at in state_rows {
        assert_eq!(game.u32(at + 8), 0);
    }
}

#[test]
fn renewal_of_a_player_who_refused_is_saved_to_the_file() {
    let dir = TempDir::new("live-refused");
    let mut data = with_request(6, 3);
    // No open request, only the state row: he refused and leaves when the contract ends.
    put_words(&mut data, FIRST_ROW + (SQUAD + 1) * boardroom::contracts::RECORD, &boardroom::contracts::BLANK);
    let mut save = open_data(&dir, data);
    save.renew_contract(club_player(MANAGED, 6), 3_000_000, date(2031, 6, 30), None, None).unwrap();
    save.write(None).unwrap();
    let again = MasterLeague::open(&save.path).unwrap();
    assert_eq!(u32_at(&again.data, again.data.len() - 0x4000 + 0x500 + 8), 0);
    assert_eq!(u32_at(&again.data, again.data.len() - 0x4000 + 0x500 + 12), STATE_ROW_X, "high byte kept");
}

#[test]
fn renewal_takes_a_player_off_hold() {
    let dir = TempDir::new("live-hold");
    let mut data = with_request(6, 2);
    // On Hold (state 2) without an open request, as Quenda in a real FL27 save.
    put_words(&mut data, FIRST_ROW + (SQUAD + 1) * boardroom::contracts::RECORD, &boardroom::contracts::BLANK);
    let mut save = open_data(&dir, data);
    save.renew_contract(club_player(MANAGED, 6), 3_000_000, date(2031, 6, 30), None, None).unwrap();
    save.write(None).unwrap();
    let again = MasterLeague::open(&save.path).unwrap();
    assert_eq!(u32_at(&again.data, again.data.len() - 0x4000 + 0x500 + 8), 0, "no longer On Hold");
}

#[test]
fn mod_studio_date_is_found_by_its_calendar() {
    let dir = TempDir::new("live-modstudio");
    let (save, game) = edited(&dir);
    game.put(TABLE + OFF_DATE, &[0; 4]);
    let today = date(GAME_DATE.0, GAME_DATE.1, GAME_DATE.2);
    let calendar = [date(2025, 7, 1).pack(), 37, date(2025, 8, 5).pack(), today.pack()];
    let mut chunk = vec![0u8; 0x4_0010];
    let at = 0x178_9478 - 0x177_FFF0 - 12;
    chunk[at..at + 16].copy_from_slice(&words(&calendar));
    game.add(TABLE + 0x177_FFF0, chunk);
    let (plan, _) = save.live_plan().unwrap();
    live::apply(&plan, &game).unwrap();
    assert_eq!(game.u32(saved(ml::FL26_FINANCE)), 800_000);
}
