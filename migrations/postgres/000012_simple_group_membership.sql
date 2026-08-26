-- One Project commercial profile can carry one Simple Group base bundle.

CREATE UNIQUE INDEX IF NOT EXISTS simple_group_projects_one_group
    ON simple_group_projects (project_id);
