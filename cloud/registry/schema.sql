-- The shared question registry (cloud/registry/README.md). Contributions are private: only the
-- maintainer reads them, with wrangler. Approved questions are what every Janus reads.
CREATE TABLE IF NOT EXISTS contributions (
    id INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- A random id per Janus install: it can withdraw everything it sent, and nothing else.
    install TEXT NOT NULL,
    -- Already rewritten on the contributor's Mac to name no person, company or product.
    text TEXT NOT NULL,
    kind TEXT NOT NULL,
    round TEXT,
    role TEXT,
    -- Only when the contributor chose to share it; never published.
    company TEXT,
    app_version TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected')),
    question_id INTEGER REFERENCES questions(id),
    UNIQUE (install, text)
);
CREATE INDEX IF NOT EXISTS contributions_by_day ON contributions (created_at);
CREATE INDEX IF NOT EXISTS contributions_by_status ON contributions (status);

CREATE TABLE IF NOT EXISTS questions (
    id INTEGER PRIMARY KEY,
    text TEXT NOT NULL,
    kind TEXT NOT NULL,
    -- JSON arrays of the rounds and roles it was asked in.
    rounds TEXT NOT NULL DEFAULT '[]',
    roles TEXT NOT NULL DEFAULT '[]',
    -- How many installs contributed it: how common it is.
    contributors INTEGER NOT NULL DEFAULT 1,
    approved_at TEXT NOT NULL DEFAULT (datetime('now'))
);
