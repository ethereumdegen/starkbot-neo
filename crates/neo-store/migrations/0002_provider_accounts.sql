CREATE TABLE provider_accounts (
  provider TEXT PRIMARY KEY,
  status TEXT NOT NULL CHECK (status IN ('signed_out','connected','rate_limited','unavailable')),
  email TEXT,
  plan_type TEXT,
  workspace TEXT,
  allowance TEXT CHECK (allowance IS NULL OR json_valid(allowance)),
  updated_at INTEGER NOT NULL
) STRICT;
