-- This records the identity chosen when each immutable revision was saved.
-- It is intentionally not a rebuildable projection: aliases may later change.
CREATE TABLE ingredient_resolution_provenance (
    revision_id TEXT NOT NULL REFERENCES recipe_revisions(id),
    ordinal INTEGER NOT NULL,
    normalized_wording TEXT NOT NULL,
    ingredient_id TEXT NOT NULL REFERENCES ingredients(id),
    PRIMARY KEY (revision_id, ordinal)
);

-- Preserve resolutions already present in pre-provenance Stage 3 databases.
INSERT INTO ingredient_resolution_provenance (
    revision_id, ordinal, normalized_wording, ingredient_id
)
SELECT revision_id, ordinal, normalized_wording, ingredient_id
FROM ingredient_occurrences;

CREATE INDEX ingredient_resolution_provenance_ingredient
    ON ingredient_resolution_provenance(ingredient_id);
