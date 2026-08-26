use axum::http::Method;

pub const DOUBAO_TASKS_PATH: &str = "/doubao/v3/contents/generations/tasks";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubaoTaskAction {
    Create,
    Get,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoubaoTaskRoute<'a> {
    pub action: DoubaoTaskAction,
    pub task_id: Option<&'a str>,
}

pub fn parse_doubao_task_route<'a>(
    method: &Method,
    request_path: &'a str,
) -> Option<DoubaoTaskRoute<'a>> {
    let path = request_path_without_query(request_path);

    if method == Method::POST && path == DOUBAO_TASKS_PATH {
        return Some(DoubaoTaskRoute {
            action: DoubaoTaskAction::Create,
            task_id: None,
        });
    }

    let task_id = path.strip_prefix(DOUBAO_TASKS_PATH)?.strip_prefix('/')?;

    if task_id.is_empty() || task_id.contains('/') {
        return None;
    }

    let action = match *method {
        Method::GET => DoubaoTaskAction::Get,
        Method::DELETE => DoubaoTaskAction::Delete,
        _ => return None,
    };

    Some(DoubaoTaskRoute {
        action,
        task_id: Some(task_id),
    })
}

pub fn doubao_task_route_path(action: DoubaoTaskAction, task_id: Option<&str>) -> Option<String> {
    match (action, task_id) {
        (DoubaoTaskAction::Create, None) => Some(DOUBAO_TASKS_PATH.to_owned()),
        (DoubaoTaskAction::Get | DoubaoTaskAction::Delete, Some(task_id))
            if !task_id.is_empty() && !task_id.contains('/') =>
        {
            Some(format!("{DOUBAO_TASKS_PATH}/{task_id}"))
        }
        _ => None,
    }
}

fn request_path_without_query(request_path: &str) -> &str {
    request_path
        .split_once('?')
        .map_or(request_path, |(path, _)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_create_task_path() {
        let route = parse_doubao_task_route(
            &Method::POST,
            "/doubao/v3/contents/generations/tasks?ignored=true",
        );

        assert_eq!(
            route,
            Some(DoubaoTaskRoute {
                action: DoubaoTaskAction::Create,
                task_id: None
            })
        );
    }

    #[test]
    fn parses_get_task_path_and_extracts_task_id() {
        let route = parse_doubao_task_route(
            &Method::GET,
            "/doubao/v3/contents/generations/tasks/task_123",
        );

        assert_eq!(
            route,
            Some(DoubaoTaskRoute {
                action: DoubaoTaskAction::Get,
                task_id: Some("task_123")
            })
        );
    }

    #[test]
    fn parses_delete_task_path_and_extracts_task_id() {
        let route = parse_doubao_task_route(
            &Method::DELETE,
            "/doubao/v3/contents/generations/tasks/task_123",
        );

        assert_eq!(
            route,
            Some(DoubaoTaskRoute {
                action: DoubaoTaskAction::Delete,
                task_id: Some("task_123")
            })
        );
    }

    #[test]
    fn rejects_unknown_action_or_shape() {
        assert_eq!(
            parse_doubao_task_route(&Method::PUT, "/doubao/v3/contents/generations/tasks"),
            None
        );
        assert_eq!(
            parse_doubao_task_route(&Method::GET, "/doubao/v3/contents/generations/tasks"),
            None
        );
        assert_eq!(
            parse_doubao_task_route(
                &Method::GET,
                "/doubao/v3/contents/generations/tasks/task_123/extra"
            ),
            None
        );
        assert_eq!(
            parse_doubao_task_route(
                &Method::POST,
                "/doubao/v3/contents/generations/tasks/task_123"
            ),
            None
        );
    }

    #[test]
    fn builds_supported_task_route_paths() {
        assert_eq!(
            doubao_task_route_path(DoubaoTaskAction::Create, None),
            Some("/doubao/v3/contents/generations/tasks".to_owned())
        );
        assert_eq!(
            doubao_task_route_path(DoubaoTaskAction::Get, Some("task_123")),
            Some("/doubao/v3/contents/generations/tasks/task_123".to_owned())
        );
        assert_eq!(
            doubao_task_route_path(DoubaoTaskAction::Delete, Some("task_123")),
            Some("/doubao/v3/contents/generations/tasks/task_123".to_owned())
        );
        assert_eq!(doubao_task_route_path(DoubaoTaskAction::Get, None), None);
    }
}
