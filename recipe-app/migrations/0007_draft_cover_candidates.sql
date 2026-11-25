CREATE TABLE draft_cover_candidates (
    image_ref TEXT PRIMARY KEY NOT NULL,
    draft_id TEXT NOT NULL,
    owner_token_hash TEXT NOT NULL,
    draft_revision INTEGER NOT NULL CHECK (draft_revision > 0),
    created_at TEXT NOT NULL,
    FOREIGN KEY (draft_id) REFERENCES recipe_drafts(id) ON DELETE CASCADE
);

CREATE INDEX draft_cover_candidates_owner_draft_revision
    ON draft_cover_candidates(owner_token_hash, draft_id, draft_revision);

-- Keep unselected/discarded candidate objects private even after the draft row
-- (and its candidate binding) is removed. The selected ref is released on publish.
CREATE TABLE private_image_refs (
    image_ref TEXT PRIMARY KEY NOT NULL
);
