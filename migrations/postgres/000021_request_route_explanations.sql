CREATE TABLE IF NOT EXISTS request_route_explanations (
    request_id BIGINT PRIMARY KEY,
    project_id BIGINT NOT NULL,
    requested_model TEXT NOT NULL,
    load_balance_strategy TEXT NOT NULL,
    selected_candidates JSONB NOT NULL DEFAULT '[]'::jsonb,
    rejected_candidates JSONB NOT NULL DEFAULT '[]'::jsonb,
    ordered_candidates JSONB NOT NULL DEFAULT '[]'::jsonb,
    final_channel_id BIGINT,
    final_model_id TEXT,
    terminal_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS request_route_explanations_by_project_id_created_at
    ON request_route_explanations (project_id, created_at);
