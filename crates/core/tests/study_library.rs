#![cfg(feature = "native")]

#[test]
fn reopening_active_library_retains_inflight_remove_recreate_and_peer_deletion() {
    let (env, mut a, mut b) = pair();
    a.set_note(verse(16), "Keep recreated").unwrap();
    sync(&mut a);
    sync(&mut b);
    let folder = a.study_library().unwrap().unwrap();
    let request = a.prepare_library_connection(folder).unwrap();
    request.preview().unwrap();
    b.set_note(verse(16), "").unwrap();
    sync(&mut b);
    let response = request.exchange().unwrap();
    a.set_note(verse(16), "").unwrap();
    a.set_note(verse(16), "Keep recreated").unwrap();
    a.finish_library_sync(response).unwrap();
    drop(a);
    let mut a = env.store("a");
    assert_eq!(a.note(verse(16)).unwrap().body, "Keep recreated");
    let conflicts = a.library_conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert!(conflicts[0].versions.iter().any(|v| v.deleted));
    sync(&mut a);
    sync(&mut b);
    assert_eq!(b.library_conflicts().unwrap()[0].versions.len(), 2);
}

#[test]
fn changing_source_during_location_change_requires_fresh_preview_without_losing_heads() {
    let (env, mut a, mut b) = pair();
    a.set_note(verse(16), "Original").unwrap();
    sync(&mut a);
    sync(&mut b);
    b.set_note(verse(16), "Remote text").unwrap();
    sync(&mut b);
    sync(&mut a);
    let original = a.study_library().unwrap().unwrap();
    let target = LibraryFolder::create(&env.0.join("target")).unwrap();
    let request = a.prepare_library_connection(target.clone()).unwrap();
    request.preview().unwrap();
    let response = request.exchange().unwrap();
    a.set_note_from(verse(16), "Draft based on Original", "Original").unwrap();
    assert!(a.finish_library_sync(response).is_err());
    drop(a);
    let mut a = env.store("a");
    assert_eq!(a.study_library().unwrap(), Some(original));
    assert_eq!(a.library_conflicts().unwrap()[0].versions.len(), 2);
    a.connect_study_library(target.clone()).unwrap();
    let mut fresh = env.store("fresh");
    fresh.connect_study_library(target).unwrap();
    let conflicts = fresh.library_conflicts().unwrap();
    let versions = &conflicts[0].versions;
    assert_eq!(versions.len(), 2);
    assert!(versions.iter().any(|v| v.text == "Remote text"));
    assert!(versions.iter().any(|v| v.text == "Draft based on Original"));
}

#[test]
fn failed_initial_exchange_retains_the_previous_connection() {
    let (env, mut a, _) = pair();
    let original = a.study_library().unwrap().unwrap();
    a.set_note(verse(16), "Original library").unwrap();
    sync(&mut a);
    let candidate = LibraryFolder::create(&env.0.join("candidate")).unwrap();
    fs::write(candidate.path().join("revisions/broken.json"), "{").unwrap();
    assert!(a.connect_study_library(candidate).is_err());
    assert_eq!(a.study_library().unwrap().unwrap(), original);
    a.set_note(verse(17), "Still editing original").unwrap();
    drop(a);
    let a = env.store("a");
    assert_eq!(a.study_library().unwrap().unwrap(), original);
    assert_eq!(a.note(verse(17)).unwrap().body, "Still editing original");
}

#[test]
fn changing_location_preserves_every_live_conflict_version() {
    let (env, mut a, mut b) = pair();
    a.set_note(verse(16), "Original").unwrap();
    sync(&mut a);
    sync(&mut b);
    a.set_note(verse(16), "Desktop").unwrap();
    b.set_note(verse(16), "Phone").unwrap();
    sync(&mut a);
    sync(&mut b);
    sync(&mut a);
    a.disconnect_study_library().unwrap();
    let new = LibraryFolder::create(&env.0.join("new-location")).unwrap();
    a.connect_study_library(new.clone()).unwrap();
    let mut fresh = env.store("fresh");
    fresh.connect_study_library(new).unwrap();
    let versions = &fresh.library_conflicts().unwrap()[0].versions;
    assert_eq!(versions.len(), 2);
    assert!(versions.iter().any(|v| v.text == "Desktop"));
    assert!(versions.iter().any(|v| v.text == "Phone"));
}

#[test]
fn connection_preview_is_read_only_and_shows_both_sides_and_conflicts() {
    let (env, mut a, mut b) = pair();
    a.set_note(verse(16), "A").unwrap();
    sync(&mut a);
    b.set_note(verse(16), "B").unwrap();
    let original = b.study_library().unwrap().unwrap();
    b.disconnect_study_library().unwrap();
    let request = b.prepare_library_connection(original.clone()).unwrap();
    let before = fs::read_dir(original.path().join("revisions")).unwrap().count();
    let preview = request.preview().unwrap();
    assert_eq!(preview.local_counts, [0, 1, 0]);
    assert_eq!(preview.incoming_counts, [0, 1, 0]);
    assert_eq!(preview.conflicts.len(), 1);
    assert_eq!(fs::read_dir(original.path().join("revisions")).unwrap().count(), before);
    assert!(b.study_library().unwrap().is_none());
    let empty = env.0.join("empty");
    fs::create_dir(&empty).unwrap();
    let planned = LibraryFolder::plan_create(&empty).unwrap();
    b.prepare_library_connection(planned).unwrap().preview().unwrap();
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
}

#[test]
fn explicit_note_merge_rejects_unreviewed_versions_and_converges() {
    let (_env, mut a, mut b) = pair();
    a.set_note(verse(16), "Original").unwrap();
    sync(&mut a);
    sync(&mut b);
    a.set_note(verse(16), "A").unwrap();
    b.set_note(verse(16), "B").unwrap();
    sync(&mut a);
    sync(&mut b);
    sync(&mut a);
    let conflict = &a.library_conflicts().unwrap()[0];
    let key = conflict.key.clone();
    let reviewed: Vec<_> = conflict.versions.iter().map(|v| v.id.clone()).collect();
    b.set_note(verse(16), "B continued").unwrap();
    sync(&mut b);
    sync(&mut a);
    assert!(a.merge_library_note(&key, "A and B", &reviewed).is_err());
    assert_eq!(a.library_conflicts().unwrap().len(), 1);
    let reviewed: Vec<_> = a.library_conflicts().unwrap()[0].versions.iter().map(|v| v.id.clone()).collect();
    a.merge_library_note(&key, "A and B continued", &reviewed).unwrap();
    sync(&mut a);
    sync(&mut b);
    assert!(b.library_conflicts().unwrap().is_empty());
    assert_eq!(b.note(verse(16)).unwrap().body, "A and B continued");
}

#[test]
fn returning_to_a_library_does_not_turn_other_library_absence_into_deletion() {
    let (env, mut a, mut peer) = pair();
    let library_a = a.study_library().unwrap().unwrap();
    a.set_note(verse(16), "Belongs to A").unwrap();
    sync(&mut a);
    sync(&mut peer);
    a.disconnect_study_library().unwrap();
    a.connect_study_library(LibraryFolder::create(&env.0.join("b")).unwrap()).unwrap();
    sync(&mut a);
    a.set_note(verse(16), "").unwrap();
    sync(&mut a);
    a.disconnect_study_library().unwrap();
    a.connect_study_library(library_a).unwrap();
    sync(&mut a);
    sync(&mut peer);
    assert_eq!(peer.note(verse(16)).unwrap().body, "Belongs to A");
    assert_eq!(a.note(verse(16)).unwrap().body, "Belongs to A");
}

#[test]
fn switching_without_an_exchange_keeps_outgoing_pending_edits() {
    let (env, mut a, mut peer) = pair();
    let library_a = a.study_library().unwrap().unwrap();
    a.set_note(verse(16), "Unsent A edit").unwrap();
    a.connect_study_library(LibraryFolder::create(&env.0.join("b")).unwrap()).unwrap();
    a.set_note(verse(16), "B edit").unwrap();
    a.connect_study_library(library_a).unwrap();
    sync(&mut a);
    sync(&mut peer);
    let versions = &peer.library_conflicts().unwrap()[0].versions;
    assert!(versions.iter().any(|v| v.text == "Unsent A edit"));
    assert!(versions.iter().any(|v| v.text == "B edit"));
}

#[test]
fn ordinary_edit_does_not_resolve_undisplayed_conflict_versions() {
    let (env, mut a, mut b) = pair();
    a.set_note(verse(16), "Original").unwrap();
    sync(&mut a);
    sync(&mut b);
    a.set_note(verse(16), "A").unwrap();
    b.set_note(verse(16), "B").unwrap();
    sync(&mut a);
    sync(&mut b);
    sync(&mut a);
    let shown = a.note(verse(16)).unwrap().body.clone();
    a.set_note(verse(16), &format!("{shown} continued")).unwrap();
    drop(a);
    let mut a = env.store("a");
    sync(&mut a);
    sync(&mut b);
    let versions = &b.library_conflicts().unwrap()[0].versions;
    assert_eq!(versions.len(), 2);
    assert!(versions.iter().any(|v| v.text == format!("{shown} continued")));
    assert!(versions.iter().any(|v| v.text == if shown == "A" { "B" } else { "A" }));
}

#[test]
fn rapid_remove_and_recreate_is_a_causal_edit_for_every_record_kind() {
    let (env, mut a, mut b) = pair();
    a.set_note(verse(16), "Same text").unwrap();
    a.set_bookmark(verse(16), Some("")).unwrap();
    a.set_highlight(verse(16), Some(HighlightColor::Blue)).unwrap();
    sync(&mut a);
    sync(&mut b);
    a.set_note(verse(16), "").unwrap();
    a.set_note(verse(16), "Same text").unwrap();
    a.set_bookmark(verse(16), None).unwrap();
    a.set_bookmark(verse(16), Some("")).unwrap();
    a.set_highlight(verse(16), None).unwrap();
    a.set_highlight(verse(16), Some(HighlightColor::Blue)).unwrap();
    b.set_note(verse(16), "").unwrap();
    b.set_bookmark(verse(16), None).unwrap();
    b.set_highlight(verse(16), None).unwrap();
    drop(a);
    let mut a = env.store("a");
    sync(&mut b);
    sync(&mut a);
    sync(&mut b);
    assert!(a.note(verse(16)).is_some());
    assert!(a.is_bookmarked(verse(16)));
    assert!(!a.highlight_parts(verse(16)).is_empty());
    assert_eq!(a.library_conflicts().unwrap().len(), 3);
}

#[test]
fn saving_a_draft_based_on_an_older_note_preserves_new_remote_text() {
    let (_env, mut a, mut b) = pair();
    a.set_note(verse(16), "Original").unwrap();
    sync(&mut a);
    sync(&mut b);
    b.set_note(verse(16), "Remote text").unwrap();
    sync(&mut b);
    sync(&mut a);
    a.set_note_from(verse(16), "Draft from old original", "Original").unwrap();
    sync(&mut a);
    sync(&mut b);
    let versions = &b.library_conflicts().unwrap()[0].versions;
    assert!(versions.iter().any(|v| v.text == "Remote text"));
    assert!(versions.iter().any(|v| v.text == "Draft from old original"));
}

#[test]
fn failed_journal_write_rolls_back_the_study_mutation() {
    let (env, mut a, _) = pair();
    a.set_note(verse(16), "Keep").unwrap();
    let id = a.study_library().unwrap().unwrap().id().to_owned();
    a.set_setting(&format!("study-library-state:{id}"), "{").unwrap();
    assert!(a.set_note(verse(16), "Must roll back").is_err());
    assert_eq!(a.note(verse(16)).unwrap().body, "Keep");
    drop(a);
    let a = env.store("a");
    assert_eq!(a.note(verse(16)).unwrap().body, "Keep");
}

use bible_core::study::sync::LibraryFolder;
use bible_core::study::{HighlightColor, StudyStore, TextSpan};
use bible_core::{BookId, VerseRef};
use std::fs;
use std::path::PathBuf;

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("bible-library-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn store(&self, name: &str) -> StudyStore {
        StudyStore::open(&self.0.join(format!("{name}.sqlite"))).unwrap()
    }
    fn folder(&self) -> LibraryFolder {
        LibraryFolder::create(&self.0.join("shared")).unwrap()
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn verse(n: u8) -> VerseRef {
    VerseRef::new(BookId::from_code("JHN").unwrap(), 3, n)
}
fn sync(s: &mut StudyStore) {
    let r = s.prepare_library_sync().unwrap().unwrap().exchange().unwrap();
    s.finish_library_sync(r).unwrap();
}
fn pair() -> (Sandbox, StudyStore, StudyStore) {
    let env = Sandbox::new();
    let f = env.folder();
    let mut a = env.store("a");
    let mut b = env.store("b");
    a.connect_study_library(f.clone()).unwrap();
    b.connect_study_library(f).unwrap();
    (env, a, b)
}

#[test]
fn two_devices_share_study_but_keep_preferences_local() {
    let (_env, mut a, mut b) = pair();
    a.set_note(verse(16), "A note").unwrap();
    a.set_bookmark(verse(17), Some("Read later")).unwrap();
    a.set_setting("app", "desktop preferences").unwrap();
    a.highlight_span(
        &TextSpan { first: verse(16), last: verse(16), start: 4, end: 10 },
        Some(HighlightColor::Pink),
        |_| Some("For God so loved the world".into()),
    )
    .unwrap();
    sync(&mut a);
    sync(&mut b);
    assert_eq!(b.note(verse(16)).unwrap().body, "A note");
    assert_eq!(b.bookmark(verse(17)).unwrap().label, "Read later");
    assert_eq!(a.highlight_parts(verse(16)), b.highlight_parts(verse(16)));
    assert!(b.setting("app").is_none());
    sync(&mut a);
    assert!(a.library_conflicts().unwrap().is_empty());
}

#[test]
fn offline_independent_edits_and_deletes_merge() {
    let (_env, mut a, mut b) = pair();
    a.set_note(verse(16), "Original").unwrap();
    a.set_highlight(verse(18), Some(HighlightColor::Blue)).unwrap();
    sync(&mut a);
    sync(&mut b);
    a.set_note(verse(16), "").unwrap();
    a.set_highlight(verse(18), None).unwrap();
    b.set_note(verse(17), "Offline phone edit").unwrap();
    sync(&mut a);
    sync(&mut b);
    sync(&mut a);
    assert!(b.note(verse(16)).is_none());
    assert!(b.highlight_parts(verse(18)).is_empty());
    assert_eq!(a.note(verse(17)).unwrap().body, "Offline phone edit");
}

#[test]
fn concurrent_notes_are_preserved_and_resolve_on_both_devices() {
    let (_env, mut a, mut b) = pair();
    a.set_note(verse(16), "Original").unwrap();
    sync(&mut a);
    sync(&mut b);
    a.set_note(verse(16), "Desktop edit").unwrap();
    b.set_note(verse(16), "Phone edit").unwrap();
    sync(&mut a);
    sync(&mut b);
    sync(&mut a);
    let conflicts = a.library_conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].versions.len(), 2);
    let chosen = conflicts[0].versions.iter().find(|v| v.text == "Phone edit").unwrap();
    a.resolve_library_conflict(&conflicts[0].key, &chosen.id).unwrap();
    sync(&mut a);
    sync(&mut b);
    assert_eq!(a.note(verse(16)).unwrap().body, "Phone edit");
    assert_eq!(b.note(verse(16)).unwrap().body, "Phone edit");
    assert!(b.library_conflicts().unwrap().is_empty());
}

#[test]
fn editing_during_file_exchange_is_not_overwritten() {
    let (_env, mut a, mut b) = pair();
    a.set_note(verse(16), "Original").unwrap();
    sync(&mut a);
    sync(&mut b);
    b.set_note(verse(16), "Remote edit").unwrap();
    sync(&mut b);
    let response = a.prepare_library_sync().unwrap().unwrap().exchange().unwrap();
    a.set_note(verse(16), "Typed while downloading").unwrap();
    a.finish_library_sync(response).unwrap();
    let versions = &a.library_conflicts().unwrap()[0].versions;
    assert!(versions.iter().any(|v| v.text == "Remote edit"));
    assert!(versions.iter().any(|v| v.text == "Typed while downloading"));
    sync(&mut a);
    sync(&mut b);
    assert_eq!(b.library_conflicts().unwrap()[0].versions.len(), 2);
}

#[test]
fn offline_journal_survives_reopen() {
    let (env, mut a, mut b) = pair();
    a.set_note(verse(16), "Durable offline note").unwrap();
    a.prepare_library_sync().unwrap();
    drop(a);
    let mut a = env.store("a");
    sync(&mut a);
    sync(&mut b);
    assert_eq!(b.note(verse(16)).unwrap().body, "Durable offline note");
    b.set_note(verse(16), "").unwrap();
    b.prepare_library_sync().unwrap();
    drop(b);
    let mut b = env.store("b");
    sync(&mut b);
    sync(&mut a);
    assert!(a.note(verse(16)).is_none());
}

#[test]
fn delete_versus_edit_is_recoverable() {
    let (_env, mut a, mut b) = pair();
    a.set_note(verse(16), "Original").unwrap();
    sync(&mut a);
    sync(&mut b);
    a.set_note(verse(16), "").unwrap();
    b.set_note(verse(16), "Keep this").unwrap();
    sync(&mut a);
    sync(&mut b);
    let conflict = &b.library_conflicts().unwrap()[0];
    assert!(conflict.versions.iter().any(|v| v.deleted));
    assert_eq!(b.note(verse(16)).unwrap().body, "Keep this");
    let deletion = conflict.versions.iter().find(|v| v.deleted).unwrap();
    b.resolve_library_conflict(&conflict.key, &deletion.id).unwrap();
    sync(&mut b);
    sync(&mut a);
    assert!(a.note(verse(16)).is_none());
    assert!(a.library_conflicts().unwrap().is_empty());
}

#[test]
fn stale_response_cannot_reconnect_or_overwrite_another_library() {
    let (env, mut a, _) = pair();
    a.set_note(verse(16), "Stay local").unwrap();
    let response = a.prepare_library_sync().unwrap().unwrap().exchange().unwrap();
    a.disconnect_study_library().unwrap();
    a.connect_study_library(LibraryFolder::create(&env.0.join("other")).unwrap()).unwrap();
    assert!(a.finish_library_sync(response).is_err());
    assert_eq!(a.note(verse(16)).unwrap().body, "Stay local");
}

#[test]
fn missing_folder_retains_local_data_and_is_not_recreated() {
    let (env, mut a, _) = pair();
    a.set_note(verse(16), "Original").unwrap();
    sync(&mut a);
    fs::rename(env.0.join("shared"), env.0.join("moved")).unwrap();
    a.set_note(verse(17), "Pending").unwrap();
    assert!(a.prepare_library_sync().unwrap().unwrap().exchange().is_err());
    assert!(!env.0.join("shared").exists());
    assert_eq!(a.note(verse(17)).unwrap().body, "Pending");
}

#[test]
fn invalid_files_do_not_modify_study_data() {
    let (env, mut a, _) = pair();
    a.set_note(verse(16), "Local").unwrap();
    let path = env.0.join("shared/revisions/broken.json");
    fs::write(path, "{").unwrap();
    assert!(a.prepare_library_sync().unwrap().unwrap().exchange().is_err());
    assert_eq!(a.note(verse(16)).unwrap().body, "Local");
}

#[test]
fn disconnected_cloud_data_is_not_deleted() {
    let (env, mut a, mut b) = pair();
    a.set_note(verse(16), "Keep in cloud").unwrap();
    sync(&mut a);
    let files = fs::read_dir(env.0.join("shared/revisions")).unwrap().count();
    a.disconnect_study_library().unwrap();
    assert!(a.study_library().unwrap().is_none());
    assert!(a.prepare_library_sync().unwrap().is_none());
    assert_eq!(fs::read_dir(env.0.join("shared/revisions")).unwrap().count(), files);
    sync(&mut b);
    assert_eq!(b.note(verse(16)).unwrap().body, "Keep in cloud");
}

#[test]
fn create_and_open_validate_destination() {
    let env = Sandbox::new();
    let path = env.0.join("unrelated");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("important.txt"), "Keep").unwrap();
    assert!(LibraryFolder::create(&path).is_err());
    assert!(LibraryFolder::open(&path).is_err());
    let f = env.folder();
    assert_eq!(LibraryFolder::open(f.path()).unwrap(), f);
    let mut m: serde_json::Value =
        serde_json::from_slice(&fs::read(f.path().join("library.json")).unwrap()).unwrap();
    m["format"] = 999.into();
    fs::write(f.path().join("library.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    assert!(LibraryFolder::open(f.path()).is_err());
}

#[test]
fn replay_does_not_duplicate_revisions() {
    let (env, mut a, mut b) = pair();
    a.set_note(verse(16), "One revision").unwrap();
    sync(&mut a);
    sync(&mut b);
    let before = fs::read_dir(env.0.join("shared/revisions")).unwrap().count();
    for _ in 0..3 {
        sync(&mut a);
        sync(&mut b);
    }
    assert_eq!(fs::read_dir(env.0.join("shared/revisions")).unwrap().count(), before);
}
