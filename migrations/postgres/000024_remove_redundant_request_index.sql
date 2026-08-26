-- PostgreSQL can scan a btree in either direction, so this index duplicates
-- the initial (project_id, created_at) index and only adds write overhead.
DROP INDEX IF EXISTS requests_by_project_created_at_desc;
