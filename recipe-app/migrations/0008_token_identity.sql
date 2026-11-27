ALTER TABLE api_tokens ADD COLUMN id TEXT;
ALTER TABLE api_tokens ADD COLUMN created_at TEXT;
ALTER TABLE api_tokens ADD COLUMN last_used_at TEXT;
ALTER TABLE api_tokens ADD COLUMN revoked INTEGER NOT NULL DEFAULT 0;
UPDATE api_tokens SET id = lower(hex(randomblob(16))), created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now');

-- Previously removed or rotated credentials can leave drafts behind. Preserve
-- those owners as revoked identities; never transfer the drafts to another token.
INSERT INTO api_tokens(token_hash, label, id, created_at, revoked)
SELECT owner, 'orphan-' || lower(hex(randomblob(16))), lower(hex(randomblob(16))),
       strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 1
FROM (
    SELECT owner_token_hash AS owner FROM recipe_drafts
    UNION SELECT owner_token_hash AS owner FROM draft_cover_candidates
)
WHERE owner NOT IN (SELECT token_hash FROM api_tokens);
CREATE UNIQUE INDEX api_tokens_id ON api_tokens(id);

ALTER TABLE recipe_drafts RENAME COLUMN owner_token_hash TO owner_token_id;
ALTER TABLE draft_cover_candidates RENAME COLUMN owner_token_hash TO owner_token_id;
UPDATE recipe_drafts SET owner_token_id = (SELECT id FROM api_tokens WHERE token_hash = owner_token_id);
UPDATE draft_cover_candidates SET owner_token_id = (SELECT id FROM api_tokens WHERE token_hash = owner_token_id);
