# Study Libraries

A Study Library is a portable folder containing notes, bookmarks and highlights (including partial-verse highlights). The Mac and iPhone apps can open the same folder through iCloud Drive without a bible-rs account. SQLite databases, downloaded translations, online caches, credentials and reader preferences remain private to each device.

## Connect devices

1. On Mac, open Settings → Data → Study Library location → Create library. Select a new, empty folder in iCloud Drive and confirm the merge. To change location later, create a library in the new empty folder, or open an existing one.
2. On iPhone, open Reading settings → Study Library location → Open Existing Library. In Files, select that same folder using the same Apple Account. Confirm that this device's existing study data should be merged.
3. Leave both apps open while iCloud delivers files. Check Shared Folder / Check now retries reconciliation. A successful exchange means the local folder was read and written; it does not establish that another device has downloaded the changes.

On iPhone, Create Study Library also accepts an empty folder exposed by Files. Folder access is retained through a device-local security-scoped bookmark. Reopen the folder if permissions are lost or it moves. A custom local folder can also be used on desktop; support for other file providers requires provider-specific validation.

Before connecting, a JSON recovery export is saved under the local application data folder's `library-recovery/`. These connection backups are separate from the automatic backup rotation. A background preview shows both devices' supported-record counts and conflicting references before confirmation. Confirmation retains the previewed library identity; replacing the destination requires a fresh preview. Canceling or failing the initial exchange retains the existing local data and connection. Initial import and the new connection are committed together after revalidation. If source data changes during a location change, the connection is rejected for a fresh preview while keeping the edits and current connection. Reopening the currently active library incorporates its latest journal. Opening an existing library merges records; absence of a record on a newly connected device is not a request to delete the shared record.

## Offline edits and conflicts

Changes save to the local database and its revision journal in the same transaction. When the shared folder is unavailable, editing still works; retry when the provider is available. Reconciliation runs in the foreground approximately every five seconds, with shared-folder I/O on a worker thread. iOS does not guarantee continuous background execution.

Independent record changes combine. Conflicting changes to the same note, bookmark or verse's highlights retain all distinct versions. Settings lists those versions and offers Keep This Version / Keep Deletion. Conflicting notes also offer Merge Text: review the originals, edit the combined result, and save it explicitly. If new versions arrive while the merge editor is open, the draft is kept and the user must review the updated versions before saving. Normal editing continues from the displayed version and preserves undisplayed competing versions. Explicit resolution incorporates all currently known heads. Saving a note draft after remote changes preserves the draft and the remote version as competing revisions.

Disconnect retains both the local study data and the shared folder. It stops journaling subsequent edits for that library. Reconnecting merges present records and retained conflicts; a record deleted while explicitly disconnected can return from the shared folder. To synchronize deletions, keep the library connected even while offline. Erasing study data while connected propagates deletions of notes, highlights and bookmarks; the Mac confirmation calls this out. Recovery exports are still independent backups.

Reading plans, history, reading position and device preferences are outside the initial sync scope. Notes remain keyed to canonical verse references and follow translation changes.

## Format and implementation

`library.json` identifies the application, format version and library UUID. `revisions/<uuid>.json` contains immutable, typed study revisions with a record key, library ID, parent revision IDs and either a payload or a deletion marker. Temporary files are ignored, and completed revision files are published by rename. Unsupported manifests, invalid records, foreign library IDs, duplicate revision identities, cycles and missing ancestors block import rather than replacing local data. Missing ancestors may simply be awaiting cloud delivery; retry later.

Each local database stores a per-library revision journal and the last materialized records in local settings. Shared files contain neither SQLite nor credentials. Journals retain revision history and deletion markers; automatic compaction is deliberately absent in this first format, so libraries with very large edit histories will grow. Back up the folder and use the application to resolve records rather than editing revision files by hand.

The core separates `prepare_library_connection` (including a read-only preview), `prepare_library_sync`, `SyncRequest::exchange` and `finish_library_sync`: callers keep SQLite access local and serialized, perform provider I/O off the UI thread, and incorporate edits made while I/O was in flight. Mac and iPhone adapters coordinate folder access through Apple's file coordinator. The iPhone adapter persists folder permission separately from the database.

The regression suite uses independent on-disk databases and covers offline changes, deletions, partial highlights, simultaneous edits, explicit resolutions, active note drafts, library switching, rapid remove/recreate, journal failure rollback, restart, invalid files, missing folders and replay. Build/UI verification in a simulator does not replace signed physical-device testing with iCloud enabled; that provider-delivery validation remains required before treating this as a stable sync release.
