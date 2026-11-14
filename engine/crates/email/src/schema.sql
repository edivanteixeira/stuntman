CREATE TABLE IF NOT EXISTS email_messages(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE, service TEXT NOT NULL, via TEXT NOT NULL, at INTEGER NOT NULL,
  from_addr TEXT NOT NULL, from_name TEXT NOT NULL DEFAULT '', rcpt TEXT NOT NULL, subject TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL DEFAULT '', lang TEXT NOT NULL DEFAULT '', text TEXT NOT NULL DEFAULT '', html TEXT NOT NULL DEFAULT '',
  headers TEXT NOT NULL DEFAULT '[]', raw BLOB, size INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL,
  status_by TEXT NOT NULL DEFAULT '', status_at INTEGER, inbox_kind TEXT NOT NULL, trace_id TEXT, provoked INTEGER NOT NULL DEFAULT 0,
  extracted TEXT NOT NULL DEFAULT '{}', links TEXT NOT NULL DEFAULT '[]', sg_id TEXT NOT NULL, custom_args TEXT NOT NULL DEFAULT '{}',
  held_until INTEGER, reason TEXT);
CREATE INDEX IF NOT EXISTS email_messages_rcpt ON email_messages(rcpt COLLATE NOCASE, at);
CREATE INDEX IF NOT EXISTS email_messages_at ON email_messages(at);
CREATE TABLE IF NOT EXISTS email_attachments(
  message_id TEXT NOT NULL REFERENCES email_messages(id) ON DELETE CASCADE, idx INTEGER NOT NULL, name TEXT NOT NULL,
  ctype TEXT NOT NULL, size INTEGER NOT NULL, sha256 TEXT NOT NULL, inline INTEGER NOT NULL, data BLOB NOT NULL,
  PRIMARY KEY(message_id, idx));
CREATE TABLE IF NOT EXISTS email_events(
  id INTEGER PRIMARY KEY AUTOINCREMENT, message_id TEXT NOT NULL REFERENCES email_messages(id) ON DELETE CASCADE,
  at INTEGER NOT NULL, type TEXT NOT NULL, by_who TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '', reply TEXT);
CREATE INDEX IF NOT EXISTS email_events_msg ON email_events(message_id);
CREATE TABLE IF NOT EXISTS email_extractor_hits(name TEXT NOT NULL, at INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS email_inboxes(
  address TEXT PRIMARY KEY COLLATE NOCASE, kind TEXT NOT NULL, created_by TEXT NOT NULL, created_at INTEGER NOT NULL,
  expires_at INTEGER, on_arrival TEXT NOT NULL DEFAULT 'accept', defer_times INTEGER NOT NULL DEFAULT 3,
  hold_for_ms INTEGER NOT NULL DEFAULT 600000, bounce_kind TEXT NOT NULL DEFAULT 'hard', steps TEXT,
  apply_to TEXT NOT NULL DEFAULT 'every', label TEXT NOT NULL DEFAULT '');
