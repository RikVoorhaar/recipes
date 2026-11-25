CREATE TABLE recipe_drafts (
    id TEXT PRIMARY KEY NOT NULL,
    owner_token_hash TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    base_recipe_id TEXT,
    base_revision_id TEXT,
    document TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX recipe_drafts_owner_updated ON recipe_drafts(owner_token_hash, updated_at);
