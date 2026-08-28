-- Optional OpenCode Session.Info provenance for wrap-up session notes.
-- Nullable columns preserve notes recorded before this migration and notes
-- written by hosts that do not supply session provenance.
ALTER TABLE session_notes ADD COLUMN session_id TEXT;
ALTER TABLE session_notes ADD COLUMN agent_id TEXT;
ALTER TABLE session_notes ADD COLUMN provider_id TEXT;
ALTER TABLE session_notes ADD COLUMN model_id TEXT;
ALTER TABLE session_notes ADD COLUMN variant TEXT;
