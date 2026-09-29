-- The admin token itself, so the database alone brings the same link back
-- after a restart. NULL for tokens stored before this column existed.
ALTER TABLE admin_tokens ADD COLUMN token TEXT;
