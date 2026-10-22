//! Portable study records. SQLite and the pending revision journal stay on this device.
//! Prepare/finish run beside study edits; only exchange touches the shared directory, off-thread.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Bookmark, Highlight, Note, StudyStore};
use crate::books::VerseRef;

const CONFIG: &str = "study-library";
const MATERIALIZED: &str = "study-library-materialized";
const FORMAT: u32 = 1;
const MAX_FILE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryFolder {
    path: PathBuf,
    id: String,
    #[serde(skip)]
    fresh: bool,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    app: String,
    format: u32,
    id: String,
}

impl LibraryFolder {
    pub fn create(path: &Path) -> Result<Self> {
        fs::create_dir_all(path).context("creating the study folder")?;
        let mut folder = Self::plan_create(path)?;
        folder.initialize()?;
        Ok(folder)
    }

    pub fn plan_create(path: &Path) -> Result<Self> {
        ensure!(
            fs::read_dir(path)?.next().is_none(),
            "Choose an empty folder to create a library; use Open existing for a library already there"
        );
        Ok(Self { path: path.canonicalize()?, id: new_id(), fresh: true })
    }

    fn initialize(&mut self) -> Result<()> {
        ensure!(
            fs::read_dir(&self.path)?.next().is_none(),
            "The selected folder is no longer empty. Choose another folder"
        );
        fs::create_dir(self.path.join("revisions"))?;
        write_atomic(
            &self.path.join("library.json"),
            &Manifest { app: "bible-rs".into(), format: FORMAT, id: self.id.clone() },
        )?;
        self.fresh = false;
        Ok(())
    }

    pub fn open(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .context("The study folder is unavailable. Locate it again; your local data is safe")?;
        let m: Manifest = read_json(&path.join("library.json"))?;
        ensure!(m.app == "bible-rs" && m.format == FORMAT, "This is not a supported bible-rs Study Library");
        validate_id(&m.id)?;
        let revisions = fs::symlink_metadata(path.join("revisions"))?;
        ensure!(
            revisions.is_dir() && !revisions.file_type().is_symlink(),
            "The library revisions folder must be a directory"
        );
        Ok(Self { path, id: m.id, fresh: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Record {
    Highlights(Vec<Highlight>),
    Note(Note),
    Bookmark(Bookmark),
}

impl Record {
    fn same_content(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Note(a), Self::Note(b)) => a.body == b.body,
            (Self::Bookmark(a), Self::Bookmark(b)) => a.label == b.label,
            (Self::Highlights(a), Self::Highlights(b)) => {
                a.len() == b.len()
                    && a.iter().zip(b).all(|(a, b)| {
                        (a.color, a.start, a.end, &a.quote) == (b.color, b.start, b.end, &b.quote)
                    })
            }
            _ => false,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Note(n) => n.body.clone(),
            Self::Bookmark(b) => {
                if b.label.is_empty() {
                    "Bookmarked".into()
                } else {
                    b.label.clone()
                }
            }
            Self::Highlights(h) => h
                .iter()
                .map(|h| {
                    format!(
                        "{} ({}–{})",
                        h.color.name(),
                        h.start,
                        h.end.map_or("end".into(), |n| n.to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Revision {
    format: u32,
    library: String,
    id: String,
    key: String,
    parents: Vec<String>,
    record: Option<Record>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    revisions: BTreeMap<String, Revision>,
    baseline: BTreeMap<String, Record>,
}

pub struct SyncRequest {
    folder: LibraryFolder,
    revisions: BTreeMap<String, Revision>,
    connection: Option<PendingConnection>,
}

struct PendingConnection {
    previous: Option<LibraryFolder>,
    state: State,
    source_revisions: Option<BTreeMap<String, Revision>>,
    source_snapshot: BTreeMap<String, Record>,
}

#[derive(Serialize)]
pub struct LibraryPreview {
    pub local_counts: [usize; 3],
    pub incoming_counts: [usize; 3],
    pub conflicts: Vec<LibraryConflict>,
}

impl LibraryPreview {
    pub fn summary(&self) -> String {
        let [lh, ln, lb] = self.local_counts;
        let [rh, rn, rb] = self.incoming_counts;
        let conflicts = if self.conflicts.is_empty() {
            "None".into()
        } else {
            self.conflicts
                .iter()
                .map(|c| format!("{} ({}, {} versions)", c.reference, c.kind, c.versions.len()))
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "On this device: {lh} highlights, {ln} notes, {lb} bookmarks\nIn selected folder: {rh} highlights, {rn} notes, {rb} bookmarks\n\nConflicts preserved for review:\n{conflicts}"
        )
    }
}

pub struct SyncResponse {
    folder: LibraryFolder,
    revisions: BTreeMap<String, Revision>,
    connection: Option<PendingConnection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictVersion {
    pub id: String,
    pub text: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryConflict {
    pub key: String,
    pub reference: String,
    pub kind: String,
    pub versions: Vec<ConflictVersion>,
}

impl SyncRequest {
    pub fn folder(&self) -> &LibraryFolder {
        &self.folder
    }

    pub fn preview(&self) -> Result<LibraryPreview> {
        let incoming = self.read_revisions()?;
        let context = self.connection.as_ref().context("This is not a connection preview")?;
        let local_counts = record_counts(&context.state.baseline);
        let incoming_counts =
            record_counts(&materialized(&State { revisions: incoming.clone(), baseline: BTreeMap::new() }));
        let mut combined = incoming;
        merge_revisions(&mut combined, self.revisions.clone())?;
        validate_graph(&combined, &self.folder.id)?;
        Ok(LibraryPreview {
            local_counts,
            incoming_counts,
            conflicts: conflicts_for(&State { revisions: combined, baseline: BTreeMap::new() })?,
        })
    }

    fn read_revisions(&self) -> Result<BTreeMap<String, Revision>> {
        if self.folder.fresh {
            ensure!(
                fs::read_dir(&self.folder.path)?.next().is_none(),
                "The selected folder is no longer empty"
            );
            return Ok(BTreeMap::new());
        }
        let current = LibraryFolder::open(&self.folder.path)?;
        ensure!(current == self.folder, "The folder now contains a different library. Reconnect explicitly");
        let mut revisions = BTreeMap::new();
        for entry in fs::read_dir(current.path.join("revisions"))? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let r: Revision = read_json(&path)?;
            ensure!(
                path.file_stem().and_then(|s| s.to_str()) == Some(r.id.as_str()),
                "Invalid revision filename"
            );
            validate_revision(&r, &current.id)?;
            if let Some(previous) = revisions.insert(r.id.clone(), r.clone()) {
                ensure!(previous == r, "Two different records have the same revision ID");
            }
        }
        Ok(revisions)
    }

    pub fn exchange(mut self) -> Result<SyncResponse> {
        if self.folder.fresh {
            self.folder.initialize()?;
        }
        let incoming = self.read_revisions()?;
        merge_revisions(&mut self.revisions, incoming)?;
        validate_graph(&self.revisions, &self.folder.id)?;
        let dir = self.folder.path.join("revisions");
        for r in self.revisions.values() {
            let path = dir.join(format!("{}.json", r.id));
            if !path.exists() {
                write_atomic(&path, r)?;
            }
        }
        Ok(SyncResponse { folder: self.folder, revisions: self.revisions, connection: self.connection })
    }
}

impl StudyStore {
    pub fn backup_study_library(&self, data_dir: &Path) -> Result<PathBuf> {
        let dir = data_dir.join("library-recovery");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("before-library-{}.json", new_id()));
        let body: serde_json::Value =
            serde_json::from_str(&self.export_json_with("", |_| None, super::Include::EVERYTHING)?)?;
        write_atomic(&path, &body)?;
        Ok(path)
    }

    pub fn set_note_from(&mut self, verse: VerseRef, body: &str, expected: &str) -> Result<()> {
        let current = self.note(verse).map_or("", |n| n.body.as_str());
        if current == expected || self.study_library()?.is_none() {
            return self.set_note(verse, body);
        }
        if current == body {
            return Ok(());
        }
        let folder = self.study_library()?.context("No Study Library connected")?;
        let mut state = self.library_state(&folder)?;
        self.capture_changes(&folder, &mut state);
        let key = format!("note:{}", verse.key());
        let parents = state
            .revisions
            .values()
            .filter(|r| {
                r.key == key
                    && match &r.record {
                        Some(Record::Note(n)) => n.body == expected,
                        None => expected.is_empty(),
                        _ => false,
                    }
            })
            .map(|r| r.id.clone())
            .collect();
        let record = (!body.trim().is_empty()).then(|| {
            Record::Note(Note {
                body: body.into(),
                created: self.note(verse).map_or_else(super::now, |n| n.created),
                updated: super::now(),
            })
        });
        let revision = Revision {
            format: FORMAT,
            library: folder.id.clone(),
            id: new_id(),
            key: key.clone(),
            parents,
            record: record.clone(),
        };
        state.revisions.insert(revision.id.clone(), revision);
        self.transaction(|store| {
            store.apply_record(&key, record.as_ref())?;
            state.baseline = store.library_snapshot();
            store.save_library_state(&folder, &state)
        })?;
        self.generation += 1;
        Ok(())
    }

    pub fn study_library(&self) -> Result<Option<LibraryFolder>> {
        self.setting(CONFIG)
            .filter(|s| !s.is_empty())
            .map(|s| serde_json::from_str(&s).context("reading the Study Library connection"))
            .transpose()
    }

    pub fn connect_study_library(&mut self, folder: LibraryFolder) -> Result<()> {
        let response = self.prepare_library_connection(folder)?.exchange()?;
        self.finish_library_sync(response)
    }

    pub fn prepare_library_connection(&mut self, folder: LibraryFolder) -> Result<SyncRequest> {
        ensure!(self.conn.is_some(), "A Study Library needs a persistent local study database");
        let previous = self.study_library()?;
        self.journal_library_changes()?;
        let mut state = self.library_state(&folder)?;
        validate_graph(&state.revisions, &folder.id)?;
        if previous.as_ref().is_none_or(|old| old.id != folder.id) {
            let snapshot = self.library_snapshot();
            let mut imports: Vec<_> = snapshot.iter().map(|(k, r)| (k.clone(), Some(r.clone()))).collect();
            let source_id = previous.as_ref().map(|f| f.id.clone()).or_else(|| self.setting(MATERIALIZED));
            if let Some(source_id) = source_id.filter(|id| id != &folder.id) {
                let source = self.library_state_id(&source_id)?;
                let keys: BTreeSet<_> = source.revisions.values().map(|r| &r.key).collect();
                for key in keys {
                    for head in heads(&source, key) {
                        if !same_record(head.record.as_ref(), source.baseline.get(key)) {
                            imports.push((key.clone(), head.record.clone()));
                        }
                    }
                }
            }
            for (key, record) in imports {
                if heads(&state, &key).iter().any(|r| same_record(r.record.as_ref(), record.as_ref())) {
                    continue;
                }
                let r = Revision {
                    format: FORMAT,
                    library: folder.id.clone(),
                    id: new_id(),
                    key,
                    parents: Vec::new(),
                    record,
                };
                state.revisions.insert(r.id.clone(), r);
            }
            state.baseline = snapshot;
        }
        let revisions = state.revisions.clone();
        let source_revisions =
            previous.as_ref().map(|f| self.library_state(f).map(|s| s.revisions)).transpose()?;
        let source_snapshot = self.library_snapshot();
        Ok(SyncRequest {
            folder,
            revisions,
            connection: Some(PendingConnection { previous, state, source_revisions, source_snapshot }),
        })
    }

    pub fn disconnect_study_library(&mut self) -> Result<()> {
        self.prepare_library_sync()?;
        self.set_setting(CONFIG, "")
    }

    pub fn prepare_library_sync(&mut self) -> Result<Option<SyncRequest>> {
        let Some(folder) = self.study_library()? else { return Ok(None) };
        let mut state = self.library_state(&folder)?;
        self.capture_changes(&folder, &mut state);
        self.save_library_state(&folder, &state)?;
        Ok(Some(SyncRequest { folder, revisions: state.revisions, connection: None }))
    }

    pub(super) fn journal_library_changes(&mut self) -> Result<()> {
        let Some(folder) = self.study_library()? else { return Ok(()) };
        let mut state = self.library_state(&folder)?;
        self.capture_changes(&folder, &mut state);
        self.save_library_state(&folder, &state)
    }

    pub fn finish_library_sync(&mut self, response: SyncResponse) -> Result<()> {
        let folder = response.folder;
        let connecting = response.connection.is_some();
        let mut state = if let Some(connection) = response.connection {
            ensure!(
                self.study_library()? == connection.previous,
                "The active library changed while connecting; try again"
            );
            self.journal_library_changes()?;
            if connection.previous.as_ref().is_some_and(|old| old.id == folder.id) {
                self.library_state(&folder)?
            } else {
                let current = connection
                    .previous
                    .as_ref()
                    .map(|f| self.library_state(f).map(|s| s.revisions))
                    .transpose()?;
                ensure!(
                    current == connection.source_revisions
                        && self.library_snapshot() == connection.source_snapshot,
                    "Study data changed while connecting. Your edits are saved; preview and connect again"
                );
                connection.state
            }
        } else {
            ensure!(
                self.study_library()?.as_ref() == Some(&folder),
                "Ignoring a response for a disconnected library"
            );
            self.library_state(&folder)?
        };
        self.capture_changes(&folder, &mut state);
        for (id, revision) in response.revisions {
            if let Some(previous) = state.revisions.insert(id, revision.clone()) {
                ensure!(previous == revision, "Conflicting revision identity");
            }
        }
        validate_graph(&state.revisions, &folder.id)?;
        self.transaction(|store| {
            let snapshot = store.library_snapshot();
            let keys: BTreeSet<_> = state.revisions.values().map(|r| r.key.clone()).collect();
            for key in keys {
                let heads = heads(&state, &key);
                let chosen = heads.iter().rev().find(|r| r.record.is_some()).or_else(|| heads.last());
                if let Some(chosen) = chosen
                    && !same_record(snapshot.get(&key), chosen.record.as_ref())
                {
                    store.apply_record(&key, chosen.record.as_ref())?;
                }
            }
            state.baseline = store.library_snapshot();
            store.save_library_state(&folder, &state)?;
            if connecting {
                store.set_setting(CONFIG, &serde_json::to_string(&folder)?)?;
            }
            store.set_setting(MATERIALIZED, &folder.id)
        })?;
        self.generation += 1;
        Ok(())
    }

    pub fn library_conflicts(&self) -> Result<Vec<LibraryConflict>> {
        let Some(folder) = self.study_library()? else { return Ok(Vec::new()) };
        let state = self.library_state(&folder)?;
        conflicts_for(&state)
    }

    pub fn resolve_library_conflict(&mut self, key: &str, revision: &str) -> Result<()> {
        let folder = self.study_library()?.context("No Study Library connected")?;
        let mut state = self.library_state(&folder)?;
        self.capture_changes(&folder, &mut state);
        let h = heads(&state, key);
        let chosen = h
            .iter()
            .find(|r| r.id == revision)
            .context("This version is no longer current; refresh the conflict")?;
        let record = chosen.record.clone();
        let r = Revision {
            format: FORMAT,
            library: folder.id.clone(),
            id: new_id(),
            key: key.into(),
            parents: h.iter().map(|r| r.id.clone()).collect(),
            record: record.clone(),
        };
        state.revisions.insert(r.id.clone(), r);
        self.transaction(|store| {
            store.apply_record(key, record.as_ref())?;
            state.baseline = store.library_snapshot();
            store.save_library_state(&folder, &state)
        })?;
        self.generation += 1;
        Ok(())
    }

    pub fn resolve_library_conflict_reviewed(
        &mut self,
        key: &str,
        revision: &str,
        reviewed: &[String],
    ) -> Result<()> {
        self.check_reviewed_versions(key, reviewed)?;
        self.resolve_library_conflict(key, revision)
    }

    pub fn merge_library_note(&mut self, key: &str, body: &str, reviewed: &[String]) -> Result<()> {
        ensure!(parse_key(key)?.0 == "note" && !body.trim().is_empty(), "Enter a nonempty merged note");
        self.check_reviewed_versions(key, reviewed)?;
        let folder = self.study_library()?.context("No Study Library connected")?;
        let mut state = self.library_state(&folder)?;
        self.capture_changes(&folder, &mut state);
        let record = Record::Note(Note { body: body.into(), created: super::now(), updated: super::now() });
        let revision = Revision {
            format: FORMAT,
            library: folder.id.clone(),
            id: new_id(),
            key: key.into(),
            parents: heads(&state, key).iter().map(|r| r.id.clone()).collect(),
            record: Some(record.clone()),
        };
        state.revisions.insert(revision.id.clone(), revision);
        self.transaction(|store| {
            store.apply_record(key, Some(&record))?;
            state.baseline = store.library_snapshot();
            store.save_library_state(&folder, &state)
        })?;
        self.generation += 1;
        Ok(())
    }

    fn check_reviewed_versions(&self, key: &str, reviewed: &[String]) -> Result<()> {
        let conflict = self
            .library_conflicts()?
            .into_iter()
            .find(|c| c.key == key)
            .context("This conflict is no longer current")?;
        let current: BTreeSet<_> = conflict.versions.iter().map(|v| &v.id).collect();
        ensure!(
            current == reviewed.iter().collect(),
            "Conflict versions changed. Review the current versions before saving; your draft is kept"
        );
        Ok(())
    }

    fn library_state(&self, folder: &LibraryFolder) -> Result<State> {
        self.library_state_id(&folder.id)
    }

    fn library_state_id(&self, id: &str) -> Result<State> {
        self.setting(&format!("study-library-state:{id}"))
            .map(|s| serde_json::from_str(&s).context("reading the local sync journal"))
            .transpose()
            .map(|s| s.unwrap_or_default())
    }

    fn save_library_state(&mut self, folder: &LibraryFolder, state: &State) -> Result<()> {
        self.set_setting(&format!("study-library-state:{}", folder.id), &serde_json::to_string(state)?)
    }

    fn library_snapshot(&self) -> BTreeMap<String, Record> {
        let mut map = BTreeMap::new();
        for (&v, h) in &self.highlights {
            map.insert(format!("highlights:{v}"), Record::Highlights(h.clone()));
        }
        for (&v, n) in &self.notes {
            map.insert(format!("note:{v}"), Record::Note(n.clone()));
        }
        for (&v, b) in &self.bookmarks {
            map.insert(format!("bookmark:{v}"), Record::Bookmark(b.clone()));
        }
        map
    }

    fn capture_changes(&self, folder: &LibraryFolder, state: &mut State) {
        let snapshot = self.library_snapshot();
        let keys: BTreeSet<_> = snapshot.keys().chain(state.baseline.keys()).cloned().collect();
        for key in keys {
            if same_record(snapshot.get(&key), state.baseline.get(&key)) {
                continue;
            }
            let r = Revision {
                format: FORMAT,
                library: folder.id.clone(),
                id: new_id(),
                key: key.clone(),
                parents: heads(state, &key)
                    .iter()
                    .filter(|r| same_record(r.record.as_ref(), state.baseline.get(&key)))
                    .map(|r| r.id.clone())
                    .collect(),
                record: snapshot.get(&key).cloned(),
            };
            state.revisions.insert(r.id.clone(), r);
        }
        state.baseline = snapshot;
    }

    fn apply_record(&mut self, key: &str, record: Option<&Record>) -> Result<()> {
        let (kind, v) = parse_key(key)?;
        match (kind, record) {
            ("highlights", Some(Record::Highlights(h))) => self.put_highlights(v, h.clone()),
            ("note", Some(Record::Note(n))) => self.put_note(v, &n.body, n.created, n.updated),
            ("bookmark", Some(Record::Bookmark(b))) => self.put_bookmark(v, &b.label, b.created),
            ("highlights", None) => self.put_highlights(v, Vec::new()),
            ("note", None) => {
                self.exec(
                    "DELETE FROM notes WHERE book = ?1 AND chapter = ?2 AND verse = ?3",
                    rusqlite::params![v.book.info().code, v.chapter, v.verse],
                )?;
                self.notes.remove(&v.key());
                Ok(())
            }
            ("bookmark", None) => {
                self.exec(
                    "DELETE FROM bookmarks WHERE book = ?1 AND chapter = ?2 AND verse = ?3",
                    rusqlite::params![v.book.info().code, v.chapter, v.verse],
                )?;
                self.bookmarks.remove(&v.key());
                Ok(())
            }
            _ => bail!("A revision has the wrong record type"),
        }
    }
}

fn conflicts_for(state: &State) -> Result<Vec<LibraryConflict>> {
    let keys: BTreeSet<_> = state.revisions.values().map(|r| &r.key).collect();
    let mut conflicts = Vec::new();
    for key in keys {
        let h = heads(state, key);
        let mut unique: Vec<&Revision> = Vec::new();
        for r in h {
            if !unique.iter().any(|a| same_record(a.record.as_ref(), r.record.as_ref())) {
                unique.push(r);
            }
        }
        if unique.len() < 2 {
            continue;
        }
        let (kind, v) = parse_key(key)?;
        conflicts.push(LibraryConflict {
            key: key.clone(),
            reference: v.to_string(),
            kind: kind.into(),
            versions: unique
                .into_iter()
                .map(|r| ConflictVersion {
                    id: r.id.clone(),
                    text: r.record.as_ref().map_or("Deleted on another device".into(), Record::describe),
                    deleted: r.record.is_none(),
                })
                .collect(),
        });
    }
    Ok(conflicts)
}

fn merge_revisions(
    target: &mut BTreeMap<String, Revision>,
    incoming: BTreeMap<String, Revision>,
) -> Result<()> {
    for (id, r) in incoming {
        if let Some(previous) = target.insert(id, r.clone()) {
            ensure!(previous == r, "Conflicting revision identity");
        }
    }
    Ok(())
}

fn materialized(state: &State) -> BTreeMap<String, Record> {
    let keys: BTreeSet<_> = state.revisions.values().map(|r| &r.key).collect();
    keys.into_iter()
        .filter_map(|key| {
            let h = heads(state, key);
            h.iter().rev().find_map(|r| r.record.as_ref()).cloned().map(|r| (key.clone(), r))
        })
        .collect()
}

fn record_counts(records: &BTreeMap<String, Record>) -> [usize; 3] {
    let mut counts = [0; 3];
    for r in records.values() {
        match r {
            Record::Highlights(h) => counts[0] += h.len(),
            Record::Note(_) => counts[1] += 1,
            Record::Bookmark(_) => counts[2] += 1,
        }
    }
    counts
}

fn heads<'a>(state: &'a State, key: &str) -> Vec<&'a Revision> {
    let revisions: Vec<_> = state.revisions.values().filter(|r| r.key == key).collect();
    let parents: BTreeSet<_> = revisions.iter().flat_map(|r| &r.parents).collect();
    revisions.into_iter().filter(|r| !parents.contains(&r.id)).collect()
}

fn same_record(a: Option<&Record>, b: Option<&Record>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.same_content(b),
        _ => false,
    }
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}
fn validate_id(id: &str) -> Result<()> {
    ensure!(Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id), "Invalid library or revision ID");
    Ok(())
}

fn parse_key(key: &str) -> Result<(&str, VerseRef)> {
    let (kind, v) = key.split_once(':').context("Invalid study record key")?;
    ensure!(matches!(kind, "note" | "bookmark" | "highlights"), "Unknown study record kind");
    let v: u32 = v.parse()?;
    ensure!(key == format!("{kind}:{v}"), "Noncanonical study record key");
    let v = VerseRef::from_key(v).context("Invalid verse reference")?;
    ensure!(v.chapter > 0 && v.chapter <= v.book.info().chapters && v.verse > 0, "Invalid verse reference");
    Ok((kind, v))
}

fn validate_revision(r: &Revision, library: &str) -> Result<()> {
    ensure!(r.format == FORMAT && r.library == library, "Unsupported or foreign study revision");
    validate_id(&r.id)?;
    let (kind, _) = parse_key(&r.key)?;
    ensure!(
        match &r.record {
            None => true,
            Some(Record::Note(n)) => kind == "note" && !n.body.trim().is_empty(),
            Some(Record::Bookmark(_)) => kind == "bookmark",
            Some(Record::Highlights(h)) =>
                kind == "highlights"
                    && !h.is_empty()
                    && h.iter().all(|h| h.end.is_none_or(|e| e > h.start))
                    && h.windows(2).all(|p| p[0].end.is_some_and(|end| end <= p[1].start)),
        },
        "Invalid study record payload"
    );
    for p in &r.parents {
        validate_id(p)?;
        ensure!(p != &r.id, "A revision cannot reference itself");
    }
    Ok(())
}

fn validate_graph(revisions: &BTreeMap<String, Revision>, library: &str) -> Result<()> {
    for (id, r) in revisions {
        ensure!(id == &r.id, "Invalid revision identity");
        validate_revision(r, library)?;
        for p in &r.parents {
            let parent = revisions
                .get(p)
                .context("Waiting for earlier revisions to download; retry when the folder is available")?;
            ensure!(parent.key == r.key, "A revision references another record");
        }
    }
    let mut children: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
    let mut counts: BTreeMap<_, _> = revisions.iter().map(|(id, r)| (id, r.parents.len())).collect();
    let mut ready: Vec<_> =
        revisions.iter().filter(|(_, r)| r.parents.is_empty()).map(|(id, _)| id).collect();
    for (id, r) in revisions {
        for parent in &r.parents {
            children.entry(parent).or_default().push(id);
        }
    }
    let mut visited = 0;
    while let Some(id) = ready.pop() {
        visited += 1;
        for child in children.get(id).into_iter().flatten() {
            let count = counts.get_mut(child).expect("known revision");
            *count -= 1;
            if *count == 0 {
                ready.push(child);
            }
        }
    }
    ensure!(visited == revisions.len(), "The study revision history contains a cycle");
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= MAX_FILE,
        "Invalid or oversized library file"
    );
    let mut bytes = Vec::new();
    File::open(path)?.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_FILE, "Library file is too large");
    serde_json::from_slice(&bytes).with_context(|| format!("Invalid library file: {}", path.display()))
}

fn write_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_file_name(format!(".{}.tmp", new_id()));
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        let bytes = serde_json::to_vec_pretty(value)?;
        ensure!(bytes.len() as u64 <= MAX_FILE, "A study revision exceeds the supported file size");
        f.write_all(&bytes)?;
        f.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
