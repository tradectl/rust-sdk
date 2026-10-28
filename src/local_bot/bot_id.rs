//! `bot/<bot>/bot-id`: who this bot folder is, and the copy, move and
//! other-host checks a start runs (section 6.2).
//!
//! A separate file, not a field in `config.json` or `bot-tag`: old versions
//! rewrite those and drop fields they do not know. They never touch this.
//!
//! A copy is refused, not silently made a separate bot: a copy has the same
//! keys and pairs, and a start adopts every position on its pairs. Two bots
//! managing one book is the over-close and phantom-close incident class.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::exit::reason;
use super::running::Liveness;
use super::Home;

pub const FILE_NAME: &str = "bot-id";
pub const BOT_ID_V: u32 = 1;
/// The engine's tag file; read here for folders that have no `bot-id` yet.
const TAG_FILE: &str = "bot-tag";
/// Files a separate copy must not share with its original: pairing and port.
const PAIRING_FILES: [&str; 6] = ["cert.pem", "key.pem", "creds.json", "published", "port", "host"];
const HOST_UNKNOWN: &str = "host id unknown";

pub type Id = String;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotIdFile {
    pub v: u32,
    /// `b` + 25 chars of lowercase base32.
    pub id: Id,
    /// The canonical folder when the id was made or last confirmed.
    pub path: String,
    /// `host_id()` then; `None` when unknown.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub created_ms: i64,
}

/// What `bot-tag` says about the folder (only the fields used here).
#[derive(Deserialize)]
struct TagFile {
    tag: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    host: Option<String>,
}

/// A fresh id: `b` + 25 base32 chars (125 random bits).
pub fn mint_id() -> Id {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut bits = rand::random::<u128>();
    let mut id = String::with_capacity(26);
    id.push('b');
    for _ in 0..25 {
        id.push(ALPHABET[(bits & 31) as usize] as char);
        bits >>= 5;
    }
    id
}

/// Bot `name`'s `bot-id`, when it exists and parses.
pub fn read(home: &Home, name: &str) -> Option<BotIdFile> {
    read_in(&home.bot_dir(name))
}

fn read_in(dir: &Path) -> Option<BotIdFile> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(FILE_NAME)).ok()?).ok()
}

fn read_tag(dir: &Path) -> Option<TagFile> {
    serde_json::from_str::<TagFile>(&std::fs::read_to_string(dir.join(TAG_FILE)).ok()?)
        .ok()
        .filter(|t| !t.tag.is_empty())
}

fn write_in(dir: &Path, f: &BotIdFile) -> io::Result<()> {
    let text = serde_json::to_string_pretty(f).map_err(io::Error::other)?;
    super::fsx::write_atomic(&dir.join(FILE_NAME), text.as_bytes(), true)
}

/// A new bot's id, written into its (existing) folder. For the skeleton.
pub fn create(home: &Home, name: &str) -> io::Result<BotIdFile> {
    let dir = std::fs::canonicalize(home.bot_dir(name))?;
    mint_in(&dir, record_host(&Env::real()))
}

fn mint_in(dir: &Path, host: Option<String>) -> io::Result<BotIdFile> {
    let f = BotIdFile {
        v: BOT_ID_V,
        id: mint_id(),
        path: dir.to_string_lossy().into_owned(),
        host,
        created_ms: super::now_ms(),
    };
    write_in(dir, &f)?;
    Ok(f)
}

/// The confirm flags of `tradectl run` (`--moved-here`, `--separate-copy`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Confirm {
    pub moved_here: bool,
    pub separate_copy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdCheck {
    /// Nothing changed.
    Same(Id),
    /// A first id for this folder.
    Minted(Id),
    /// The folder moved (its original is gone, or is another bot now).
    Moved(Id),
    /// `--separate-copy`: a new id, its own pairing and port.
    Separated { new: Id, of: PathBuf },
    /// Refused: a copy of the stopped bot at `of`.
    Copy { of: PathBuf },
    /// Refused: a copy of `orig`, which is running.
    CopyOfLive { orig: String },
    /// Refused: the folder came from another computer.
    OtherHost { was: String, now: String },
}

impl IdCheck {
    /// The id to run with; `None` for a refusal.
    pub fn id(&self) -> Option<&str> {
        match self {
            IdCheck::Same(id) | IdCheck::Minted(id) | IdCheck::Moved(id) => Some(id),
            IdCheck::Separated { new, .. } => Some(new),
            IdCheck::Copy { .. } | IdCheck::CopyOfLive { .. } | IdCheck::OtherHost { .. } => None,
        }
    }

    /// The `start-result: refused reason=` value (exit 78).
    pub fn refusal(&self) -> Option<&'static str> {
        match self {
            IdCheck::Copy { .. } => Some(reason::COPY),
            IdCheck::CopyOfLive { .. } => Some(reason::COPY_OF_LIVE),
            IdCheck::OtherHost { .. } => Some(reason::MOVED_HOST),
            _ => None,
        }
    }

    /// The refusal text.
    pub fn message(&self) -> Option<String> {
        match self {
            IdCheck::Copy { of } => Some(format!(
                "This folder is a copy of {}. To make it a separate bot, start it with --separate-copy \
                 (it uses the same exchange keys and pairs: if both run, both trade the same positions).",
                of.display()
            )),
            IdCheck::CopyOfLive { orig } => {
                Some(format!("This folder is a copy of {orig}, which is running. Stop it first, or delete this copy."))
            }
            IdCheck::OtherHost { .. } => Some(
                "This bot was copied from another computer. Stop it there first, then start it here with --moved-here."
                    .to_string(),
            ),
            _ => None,
        }
    }
}

/// The result and the status notes it adds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub check: IdCheck,
    pub notes: Vec<String>,
}

/// The holder's identity check (8.1 step 7), run under the bot's own lock
/// once the folder exists. A `bot-id` is written only when the start may go
/// on; a refusal leaves the folder as it was.
pub fn check_at_start(home: &Home, name: &str, confirm: Confirm) -> io::Result<Checked> {
    check_with(home, name, confirm, &Env::real())
}

pub(crate) struct Env<'a> {
    pub host_now: Option<String>,
    pub container: bool,
    /// `bot_running_now`, faked in tests.
    pub running: &'a dyn Fn(&Home, &str) -> Liveness,
}

impl Env<'static> {
    fn real() -> Env<'static> {
        Env {
            host_now: super::host_id::host_id(),
            container: super::host_id::in_container(),
            running: &super::running::bot_running_now,
        }
    }
}

/// The host to record: none inside a container, where it may change.
fn record_host(env: &Env) -> Option<String> {
    env.host_now.clone().filter(|_| !env.container)
}

enum Key {
    Id(Id),
    Tag(String),
}

/// The folder's recorded identity, read before anything is rewritten: from
/// `bot-id`, else from `bot-tag` (every v0.2.x start writes it).
struct Recorded {
    path: PathBuf,
    host: Option<String>,
    key: Key,
    file: Option<BotIdFile>,
}

fn recorded(dir: &Path) -> Option<Recorded> {
    if let Some(f) = read_in(dir) {
        return Some(Recorded {
            path: PathBuf::from(&f.path),
            host: f.host.clone(),
            key: Key::Id(f.id.clone()),
            file: Some(f),
        });
    }
    let t = read_tag(dir)?;
    Some(Recorded { path: PathBuf::from(t.path), host: t.host, key: Key::Tag(t.tag), file: None })
}

/// Folder `o` has the same key (an equal tag means a copy: tags are minted
/// unique among the siblings of one bots folder).
fn same_key(o: &Path, key: &Key) -> bool {
    match key {
        Key::Id(id) => read_in(o).is_some_and(|f| f.id == *id),
        Key::Tag(tag) => read_tag(o).is_some_and(|t| t.tag == *tag),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// `<home>/bot/<name>` → (`<home>`, `<name>`).
fn original_of(o: &Path) -> Option<(Home, String)> {
    let name = o.file_name()?.to_str()?.to_string();
    let bots = o.parent()?;
    if bots.file_name()? != "bot" {
        return None;
    }
    Some((Home::at(bots.parent()?), name))
}

/// Section 6.2. Rows 1-3 (copies) first, then the host rows 6-8, then the
/// rows that write (4, 5, 9-11): so no id is minted or moved before every
/// refusal had its say, and a folder moved from another computer is
/// refused even when its path changed too.
pub(crate) fn check_with(home: &Home, name: &str, confirm: Confirm, env: &Env) -> io::Result<Checked> {
    let this = std::fs::canonicalize(home.bot_dir(name))?;
    let mut notes = Vec::new();
    let host_known = env.host_now.is_some() && !env.container;
    let refuse = |check: IdCheck, notes: Vec<String>| Ok(Checked { check, notes });

    // Row 10: no recorded identity at all.
    let Some(rec) = recorded(&this) else {
        if !host_known {
            notes.push(HOST_UNKNOWN.to_string());
        }
        let f = mint_in(&this, record_host(env))?;
        return Ok(Checked { check: IdCheck::Minted(f.id), notes });
    };
    let elsewhere = !same_path(&rec.path, &this);
    let original = if elsewhere { original_of(&rec.path) } else { None };
    let is_self = |h: &Home, n: &str| n == name && same_path(h.root(), home.root());

    // Row 1: a copy of a live bot (legacy included).
    if let Some((oh, on)) = &original {
        if !is_self(oh, on) && (env.running)(oh, on).alive() {
            return refuse(IdCheck::CopyOfLive { orig: on.clone() }, notes);
        }
    }
    // Row 2: a verified holder of this home (or the original's) runs our id.
    if let Key::Id(id) = &rec.key {
        let mut homes = vec![home.clone()];
        if let Some((oh, _)) = &original {
            if !same_path(oh.root(), home.root()) {
                homes.push(oh.clone());
            }
        }
        for h in &homes {
            for r in super::holder::read_all(h) {
                if r.bot_id.as_deref() != Some(id.as_str()) || is_self(h, &r.name) {
                    continue;
                }
                if matches!((env.running)(h, &r.name), Liveness::Running(l) if l.bot_id.as_deref() == Some(id.as_str()))
                {
                    return refuse(IdCheck::CopyOfLive { orig: r.name }, notes);
                }
            }
        }
    }
    // Row 3: a copy of a stopped bot whose folder still has our key.
    let copy_of = (elsewhere && rec.path.is_dir() && same_key(&rec.path, &rec.key)).then(|| rec.path.clone());
    if let Some(o) = &copy_of {
        if !confirm.separate_copy {
            return refuse(IdCheck::Copy { of: o.clone() }, notes);
        }
    }
    // Rows 6-8: the host.
    let mut host_confirmed = false;
    if !host_known {
        notes.push(HOST_UNKNOWN.to_string());
    } else if let (Some(was), Some(now)) = (&rec.host, &env.host_now) {
        if was != now {
            if !confirm.moved_here {
                return refuse(IdCheck::OtherHost { was: was.clone(), now: now.clone() }, notes);
            }
            host_confirmed = true;
        }
    }

    // Row 4: confirmed separate copy.
    if let Some(o) = copy_of {
        // Files first: a failure here leaves the copy refused as a copy.
        for file in PAIRING_FILES {
            match std::fs::remove_file(this.join(file)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        let f = mint_in(&this, record_host(env))?;
        // The original's order tags, after the new id (which keeps a failed
        // attempt refused for a tag-only copy): kept, they read as this bot's
        // own or older tags, and a stop between runs would sweep the
        // original's symbols. The engine mints a tag of its own.
        match std::fs::remove_file(this.join(TAG_FILE)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        notes.push(format!("made a separate bot from {}; it uses the same exchange keys and pairs", o.display()));
        return Ok(Checked { check: IdCheck::Separated { new: f.id, of: o }, notes });
    }
    // The host to keep in a rewritten file.
    let host_after = |old: Option<String>| {
        if host_confirmed || old.is_none() {
            record_host(env).or(old)
        } else {
            old
        }
    };
    // Row 5: moved; keep the id, or mint one for a folder with only a tag.
    if elsewhere {
        let id = match rec.file {
            Some(mut f) => {
                f.path = this.to_string_lossy().into_owned();
                f.host = host_after(f.host.take());
                write_in(&this, &f)?;
                f.id
            }
            None => mint_in(&this, record_host(env))?.id,
        };
        return Ok(Checked { check: IdCheck::Moved(id), notes });
    }
    match (rec.key, rec.file) {
        // Row 9: a v0.2.7 folder's first v0.2.8 start.
        (Key::Tag(_), _) => Ok(Checked { check: IdCheck::Minted(mint_in(&this, record_host(env))?.id), notes }),
        // Row 11, with row 7's rewrite and a host seen for the first time.
        (Key::Id(id), Some(mut f)) => {
            let host = host_after(f.host.clone());
            if host != f.host {
                f.host = host;
                f.path = this.to_string_lossy().into_owned();
                write_in(&this, &f)?;
            }
            Ok(Checked { check: IdCheck::Same(id), notes })
        }
        (Key::Id(id), None) => Ok(Checked { check: IdCheck::Same(id), notes }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_bot::holder::{Holder, Mode};
    use crate::local_bot::status::Last;
    use crate::local_bot::testutil::TempHome;
    use std::cell::RefCell;

    struct World {
        live: RefCell<Vec<(PathBuf, String, Liveness)>>,
    }

    impl World {
        fn new() -> World {
            World { live: RefCell::new(Vec::new()) }
        }
        fn set(&self, home: &Home, name: &str, l: Liveness) {
            let mut live = self.live.borrow_mut();
            live.retain(|(h, n, _)| !(h == home.root() && n == name));
            live.push((home.root().to_path_buf(), name.to_string(), l));
        }
        fn get(&self, home: &Home, name: &str) -> Liveness {
            self.live
                .borrow()
                .iter()
                .find(|(h, n, _)| h == home.root() && n == name)
                .map(|(_, _, l)| l.clone())
                .unwrap_or(Liveness::NotRunning(Last::Never))
        }
    }

    fn check(t: &TempHome, w: &World, name: &str, confirm: Confirm, host: Option<&str>, container: bool) -> Checked {
        let running = |h: &Home, n: &str| w.get(h, n);
        let env = Env { host_now: host.map(str::to_string), container, running: &running };
        check_with(&t.home, name, confirm, &env).unwrap()
    }

    fn run(t: &TempHome, w: &World, name: &str, host: &str) -> IdCheck {
        check(t, w, name, Confirm::default(), Some(host), false).check
    }

    fn tag_file(dir: &Path, tag: &str, path: &Path, host: Option<&str>) {
        let j = serde_json::json!({ "tag": tag, "history": [], "adopted_tags": [], "path": path, "host": host });
        std::fs::write(dir.join(TAG_FILE), j.to_string()).unwrap();
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }

    /// An original bot: with a `bot-id` (v0.2.8) or only a `bot-tag` (v0.2.7).
    fn original(t: &TempHome, w: &World, name: &str, with_id: bool) -> PathBuf {
        let dir = t.bot(name);
        if with_id {
            assert!(matches!(run(t, w, name, "h1"), IdCheck::Minted(_)));
        } else {
            tag_file(&dir, &format!("t-{name}"), &dir, Some("h1"));
        }
        dir
    }

    fn files(dir: &Path) -> (Option<String>, Option<String>) {
        (std::fs::read_to_string(dir.join(FILE_NAME)).ok(), std::fs::read_to_string(dir.join(TAG_FILE)).ok())
    }

    fn holder_rec(name: &str, id: &str) -> Holder {
        let mut h = Holder::for_this_process(name, Some(id.into()), Mode::DaemonChild, "0.2.8");
        h.pid = 4242;
        h
    }

    #[test]
    fn order_of_rows() {
        for with_id in [true, false] {
            let t = TempHome::new();
            let w = World::new();
            let orig = original(&t, &w, "orig", with_id);

            // Row 10 and 11: a new folder mints; the next start is Same.
            t.bot("fresh");
            let IdCheck::Minted(id) = run(&t, &w, "fresh", "h1") else { panic!() };
            assert_eq!(run(&t, &w, "fresh", "h1"), IdCheck::Same(id.clone()));
            let f = read(&t.home, "fresh").unwrap();
            assert_eq!((f.v, f.host.as_deref(), f.id.len()), (1, Some("h1"), 26));
            assert_eq!(PathBuf::from(&f.path), t.home.bot_dir("fresh"));

            // Row 1: a copy of a running original, refused before anything is rewritten.
            let copy = t.home.bot_dir("copy1");
            copy_dir(&orig, &copy);
            let before = files(&copy);
            w.set(&t.home, "orig", Liveness::Starting { why: "x".into() });
            assert_eq!(run(&t, &w, "copy1", "h1"), IdCheck::CopyOfLive { orig: "orig".into() }, "with_id={with_id}");
            assert_eq!(files(&copy), before, "nothing rewritten");
            // Even with every confirm, and a legacy original counts too.
            let old = crate::local_bot::legacy::LegacyProc { pid: 9, verified: false, start: None, why: String::new() };
            w.set(&t.home, "orig", Liveness::Legacy(old));
            let c = check(&t, &w, "copy1", Confirm { moved_here: true, separate_copy: true }, Some("h1"), false);
            assert_eq!(c.check.refusal(), Some(reason::COPY_OF_LIVE));
            w.live.borrow_mut().clear();

            // Row 3: a copy of a stopped one.
            assert_eq!(run(&t, &w, "copy1", "h1"), IdCheck::Copy { of: orig.clone() });
            assert_eq!(files(&copy), before);
            // Row 3 comes before the host rows.
            assert_eq!(run(&t, &w, "copy1", "h2"), IdCheck::Copy { of: orig.clone() });

            // Row 4: --separate-copy gives a new id and drops pairing files
            // and the original's order tags.
            for f in PAIRING_FILES {
                std::fs::write(copy.join(f), "x").unwrap();
                std::fs::write(orig.join(f), "x").unwrap();
            }
            if with_id {
                // Every engine run writes one beside the id.
                tag_file(&orig, "t-orig", &orig, Some("h1"));
                std::fs::copy(orig.join(TAG_FILE), copy.join(TAG_FILE)).unwrap();
            }
            let c = check(&t, &w, "copy1", Confirm { separate_copy: true, ..Confirm::default() }, Some("h1"), false);
            let IdCheck::Separated { new, of } = c.check else { panic!("{c:?}") };
            assert_eq!(of, orig);
            assert!(c.notes[0].contains("separate bot"));
            assert_ne!(Some(new.clone()), read(&t.home, "orig").map(|f| f.id));
            for f in PAIRING_FILES {
                assert!(!copy.join(f).exists(), "{f} removed in the copy");
                assert!(orig.join(f).exists(), "{f} kept in the original");
            }
            assert!(!copy.join(TAG_FILE).exists(), "the original's tags are not the copy's");
            assert!(read_tag(&orig).is_some(), "the original keeps its tag");
            assert_eq!(run(&t, &w, "copy1", "h1"), IdCheck::Same(new), "a separate bot now");

            // Row 5: moved (the original folder is gone).
            let moved = t.home.bot_dir("moved");
            copy_dir(&orig, &moved);
            let old_id = read(&t.home, "orig").map(|f| f.id);
            let gone = t.home.root().join("gone-orig");
            std::fs::rename(&orig, &gone).unwrap();
            match run(&t, &w, "moved", "h1") {
                IdCheck::Moved(id) => {
                    if with_id {
                        assert_eq!(Some(id.clone()), old_id, "the id moves with the folder");
                    }
                    let f = read(&t.home, "moved").unwrap();
                    assert_eq!((f.id, PathBuf::from(f.path)), (id, moved.clone()));
                }
                other => panic!("{other:?}"),
            }
            assert!(matches!(run(&t, &w, "moved", "h1"), IdCheck::Same(_)));

            // Row 5: another key at the original's path is a move too (a new
            // bot made there later).
            std::fs::rename(&gone, &orig).unwrap();
            let moved2 = t.home.bot_dir("moved2");
            copy_dir(&orig, &moved2);
            if with_id {
                let mut f = read(&t.home, "orig").unwrap();
                f.id = mint_id();
                write_in(&orig, &f).unwrap();
            } else {
                tag_file(&orig, "another-tag", &orig, Some("h1"));
            }
            assert!(matches!(run(&t, &w, "moved2", "h1"), IdCheck::Moved(_)));

            // Rows 6, 7: another host; refused until confirmed, nothing written before.
            let other = t.bot("other");
            if with_id {
                write_in(
                    &other,
                    &BotIdFile {
                        v: 1,
                        id: "bother".into(),
                        path: other.to_string_lossy().into(),
                        host: Some("h0".into()),
                        created_ms: 5,
                    },
                )
                .unwrap();
            } else {
                tag_file(&other, "t-other", &other, Some("h0"));
            }
            let before = files(&other);
            assert_eq!(run(&t, &w, "other", "h1"), IdCheck::OtherHost { was: "h0".into(), now: "h1".into() });
            assert_eq!(files(&other), before);
            let c = check(&t, &w, "other", Confirm { moved_here: true, ..Confirm::default() }, Some("h1"), false);
            if with_id {
                assert_eq!(c.check, IdCheck::Same("bother".into()));
                assert_eq!(read(&t.home, "other").unwrap().created_ms, 5, "same file, new host");
            } else {
                assert!(matches!(c.check, IdCheck::Minted(_)), "row 9");
            }
            assert_eq!(read(&t.home, "other").unwrap().host.as_deref(), Some("h1"));
            assert_eq!(run(&t, &w, "other", "h1").refusal(), None);

            // A folder moved from another computer under another path is refused too.
            let far = t.bot("far");
            let elsewhere = t.home.root().join("bot-on-the-other-computer");
            if with_id {
                write_in(
                    &far,
                    &BotIdFile {
                        v: 1,
                        id: "bfar".into(),
                        path: elsewhere.to_string_lossy().into(),
                        host: Some("h0".into()),
                        created_ms: 0,
                    },
                )
                .unwrap();
            } else {
                tag_file(&far, "t-far", &elsewhere, Some("h0"));
            }
            assert_eq!(run(&t, &w, "far", "h1"), IdCheck::OtherHost { was: "h0".into(), now: "h1".into() });
            let c = check(&t, &w, "far", Confirm { moved_here: true, ..Confirm::default() }, Some("h1"), false);
            assert!(matches!(c.check, IdCheck::Moved(_)), "{c:?}");

            // Row 8: unknown host or a container: no host check, a note.
            let boxed = t.bot("boxed");
            tag_file(&boxed, "t-boxed", &boxed, Some("h0"));
            let c = check(&t, &w, "boxed", Confirm::default(), Some("h1"), true);
            assert!(matches!(c.check, IdCheck::Minted(_)));
            assert_eq!(c.notes, [HOST_UNKNOWN]);
            assert_eq!(read(&t.home, "boxed").unwrap().host, None, "no host recorded in a container");
            let c = check(&t, &w, "boxed", Confirm::default(), None, false);
            assert!(matches!(c.check, IdCheck::Same(_)));
            assert_eq!(c.notes, [HOST_UNKNOWN]);

            // A host seen for the first time is recorded, not a move.
            let c = check(&t, &w, "boxed", Confirm::default(), Some("h1"), false);
            assert!(matches!(c.check, IdCheck::Same(_)) && c.notes.is_empty());
            assert_eq!(read(&t.home, "boxed").unwrap().host.as_deref(), Some("h1"));
        }
    }

    /// Row 2: the id is held, under another name, by a verified holder.
    #[test]
    fn a_live_holder_of_our_id_refuses_the_start() {
        let t = TempHome::new();
        let w = World::new();
        t.bot("paper");
        let IdCheck::Minted(id) = run(&t, &w, "paper", "h1") else { panic!() };
        std::fs::create_dir_all(t.home.locks_dir()).unwrap();
        let rec = holder_rec("renamed", &id);
        std::fs::write(t.home.holder_path("renamed"), serde_json::to_string(&rec).unwrap()).unwrap();
        // A record alone (not verified) is not enough.
        assert_eq!(run(&t, &w, "paper", "h1"), IdCheck::Same(id.clone()));
        w.set(&t.home, "renamed", Liveness::Running(rec));
        assert_eq!(run(&t, &w, "paper", "h1"), IdCheck::CopyOfLive { orig: "renamed".into() });
        // Our own record never counts.
        w.live.borrow_mut().clear();
        let mine = holder_rec("paper", &id);
        std::fs::remove_file(t.home.holder_path("renamed")).unwrap();
        std::fs::write(t.home.holder_path("paper"), serde_json::to_string(&mine).unwrap()).unwrap();
        w.set(&t.home, "paper", Liveness::Running(mine));
        assert_eq!(run(&t, &w, "paper", "h1"), IdCheck::Same(id));
    }

    /// `--separate-copy` that cannot drop the copied pairing files keeps the
    /// copy's id, so the next start still refuses it as a copy.
    #[test]
    fn a_failed_separate_copy_stays_a_copy() {
        let t = TempHome::new();
        let w = World::new();
        let orig = original(&t, &w, "orig", true);
        let copy = t.home.bot_dir("copy");
        copy_dir(&orig, &copy);
        // A pairing file that cannot be removed: a folder by that name.
        std::fs::create_dir_all(copy.join("creds.json").join("x")).unwrap();
        let before = files(&copy);
        let separate = Confirm { separate_copy: true, ..Confirm::default() };
        let r = check_with(&t.home, "copy", separate, &Env {
            host_now: Some("h1".into()),
            container: false,
            running: &|h: &Home, n: &str| w.get(h, n),
        });
        assert!(r.is_err(), "{r:?}");
        assert_eq!(files(&copy), before, "no new id");
        assert_eq!(run(&t, &w, "copy", "h1"), IdCheck::Copy { of: orig });
    }

    #[test]
    fn copy_of_live_in_another_home() {
        let a = TempHome::new();
        let b = TempHome::new();
        let w = World::new();
        let orig = original(&a, &w, "paper", true);
        copy_dir(&orig, &b.home.bot_dir("paper"));
        w.set(&a.home, "paper", Liveness::Running(holder_rec("paper", "x")));
        assert_eq!(run(&b, &w, "paper", "h1"), IdCheck::CopyOfLive { orig: "paper".into() });
        w.live.borrow_mut().clear();
        assert_eq!(run(&b, &w, "paper", "h1"), IdCheck::Copy { of: orig });
    }

    #[test]
    fn ids_look_right() {
        let a = mint_id();
        assert_eq!(a.len(), 26);
        assert!(a.starts_with('b'));
        assert!(a.bytes().all(|c| c.is_ascii_lowercase() || (b'2'..=b'7').contains(&c)));
        assert_ne!(a, mint_id());
    }

    #[test]
    fn create_writes_a_new_id_for_a_new_bot() {
        let t = TempHome::new();
        t.bot("paper");
        let f = create(&t.home, "paper").unwrap();
        assert_eq!(read(&t.home, "paper"), Some(f));
        assert!(create(&t.home, "missing").is_err(), "the folder must exist");
    }

    #[test]
    fn refusals_carry_reasons_and_text() {
        let c = IdCheck::Copy { of: "/h/bot/a".into() };
        assert_eq!((c.refusal(), c.id()), (Some(reason::COPY), None));
        assert!(c.message().unwrap().contains("This folder is a copy of /h/bot/a."));
        let c = IdCheck::CopyOfLive { orig: "a".into() };
        assert!(c.message().unwrap().contains("which is running"));
        let c = IdCheck::OtherHost { was: "x".into(), now: "y".into() };
        assert_eq!(c.refusal(), Some(reason::MOVED_HOST));
        assert_eq!(IdCheck::Moved("b1".into()).id(), Some("b1"));
        assert_eq!(IdCheck::Same("b1".into()).message(), None);
    }
}
