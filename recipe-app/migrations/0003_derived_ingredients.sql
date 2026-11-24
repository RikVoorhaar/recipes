CREATE TABLE ingredients (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL UNIQUE,
    redirect_to TEXT REFERENCES ingredients(id),
    deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1))
);

CREATE TABLE ingredient_aliases (
    normalized_alias TEXT PRIMARY KEY NOT NULL,
    alias TEXT NOT NULL,
    ingredient_id TEXT NOT NULL REFERENCES ingredients(id)
);

CREATE TABLE ingredient_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ingredient_id TEXT NOT NULL,
    action TEXT NOT NULL,
    name TEXT NOT NULL,
    details TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    author_label TEXT NOT NULL
);

CREATE TABLE ingredient_occurrences (
    revision_id TEXT NOT NULL REFERENCES recipe_revisions(id),
    ordinal INTEGER NOT NULL,
    wording TEXT NOT NULL,
    normalized_wording TEXT NOT NULL,
    ingredient_id TEXT NOT NULL REFERENCES ingredients(id),
    PRIMARY KEY (revision_id, ordinal)
);
CREATE INDEX ingredient_occurrences_ingredient ON ingredient_occurrences(ingredient_id, revision_id);
CREATE INDEX ingredient_occurrences_revision ON ingredient_occurrences(revision_id);
CREATE INDEX ingredient_aliases_ingredient ON ingredient_aliases(ingredient_id);
