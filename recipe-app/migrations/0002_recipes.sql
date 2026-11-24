CREATE TABLE api_tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    label TEXT NOT NULL
);

CREATE TABLE recipes (
    id TEXT PRIMARY KEY NOT NULL,
    current_revision_id TEXT NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1))
);

CREATE TABLE recipe_revisions (
    id TEXT PRIMARY KEY NOT NULL,
    recipe_id TEXT NOT NULL REFERENCES recipes(id),
    created_at TEXT NOT NULL,
    author_label TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    servings INTEGER NOT NULL CHECK (servings > 0),
    tags TEXT NOT NULL,
    source_url TEXT,
    notes TEXT,
    body TEXT NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1))
);

CREATE INDEX recipe_revisions_recipe_created
    ON recipe_revisions(recipe_id, created_at);
