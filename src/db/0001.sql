PRAGMA foreign_keys = ON;

CREATE TABLE arkos_snapshots(
  id           INTEGER PRIMARY KEY,
  volume       TEXT    NOT NULL,
  root         TEXT    NOT NULL,
  label        TEXT,
  mode         TEXT    NOT NULL CHECK(mode IN ('mft','walk')),
  elevated     INTEGER NOT NULL CHECK(elevated IN (0,1)),
  state        TEXT    NOT NULL CHECK(state IN ('running','complete','failed','abandoned')),
  started_at   INTEGER NOT NULL,
  finished_at  INTEGER,
  files        INTEGER NOT NULL DEFAULT 0,
  dirs         INTEGER NOT NULL DEFAULT 0,
  bytes        INTEGER NOT NULL DEFAULT 0,
  denied       INTEGER NOT NULL DEFAULT 0,
  skipped      INTEGER NOT NULL DEFAULT 0,
  scan_ms      INTEGER
);
CREATE INDEX arkos_snapshots_volume_at ON arkos_snapshots(volume, started_at DESC);
CREATE UNIQUE INDEX arkos_snapshots_one_running ON arkos_snapshots(volume) WHERE state = 'running';

CREATE TABLE arkos_dirs(
  id          INTEGER PRIMARY KEY,
  snapshot_id INTEGER NOT NULL REFERENCES arkos_snapshots(id) ON DELETE CASCADE,
  parent_id   INTEGER REFERENCES arkos_dirs(id),
  path        TEXT    NOT NULL,
  name        TEXT    NOT NULL,
  depth       INTEGER NOT NULL,
  mtime       INTEGER,
  bytes       INTEGER NOT NULL DEFAULT 0,
  files       INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX arkos_dirs_path ON arkos_dirs(snapshot_id, path);
CREATE INDEX arkos_dirs_depth ON arkos_dirs(snapshot_id, depth DESC);
CREATE INDEX arkos_dirs_bytes ON arkos_dirs(snapshot_id, bytes DESC);

CREATE TABLE arkos_files(
  snapshot_id INTEGER NOT NULL REFERENCES arkos_snapshots(id) ON DELETE CASCADE,
  dir_id      INTEGER NOT NULL REFERENCES arkos_dirs(id) ON DELETE CASCADE,
  name        TEXT    NOT NULL,
  size        INTEGER NOT NULL,
  mtime       INTEGER NOT NULL,
  ext         TEXT,
  PRIMARY KEY(snapshot_id, dir_id, name)
) WITHOUT ROWID;
CREATE INDEX arkos_files_size  ON arkos_files(snapshot_id, size DESC);
CREATE INDEX arkos_files_mtime ON arkos_files(snapshot_id, mtime);
CREATE INDEX arkos_files_ext   ON arkos_files(snapshot_id, ext, size DESC);
