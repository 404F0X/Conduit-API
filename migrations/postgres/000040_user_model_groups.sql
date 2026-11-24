-- Independent user classification and key model scope; never infer legacy grants.
CREATE TABLE model_groups (
 id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0),
 description TEXT, status TEXT NOT NULL DEFAULT 'enabled' CHECK (status IN ('enabled','disabled','archived')),
 revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE model_group_models (
 model_group_id TEXT NOT NULL REFERENCES model_groups(id), public_model_key TEXT NOT NULL,
 PRIMARY KEY(model_group_id,public_model_key)
);
CREATE TABLE user_groups (
 id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0), description TEXT,
 status TEXT NOT NULL DEFAULT 'enabled' CHECK (status IN ('enabled','disabled','archived')),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE user_group_users (
 user_id BIGINT PRIMARY KEY REFERENCES users(id), user_group_id TEXT NOT NULL REFERENCES user_groups(id)
);
CREATE INDEX user_group_users_group ON user_group_users(user_group_id);
CREATE TABLE user_group_model_groups (
 user_group_id TEXT NOT NULL REFERENCES user_groups(id), model_group_id TEXT NOT NULL REFERENCES model_groups(id),
 PRIMARY KEY(user_group_id,model_group_id)
);
CREATE TABLE api_key_model_groups (
 api_key_id BIGINT PRIMARY KEY REFERENCES api_keys(id), model_group_id TEXT NOT NULL REFERENCES model_groups(id)
);
