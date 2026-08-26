#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JinaEndpointKind {
    Embedding,
    Rerank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JinaRequestPolicy {
    pub endpoint: JinaEndpointKind,
    pub stream_allowed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JinaRouteParseError {
    UnknownPath,
    StreamNotSupported,
}

impl JinaRequestPolicy {
    pub const fn no_stream(endpoint: JinaEndpointKind) -> Self {
        Self {
            endpoint,
            stream_allowed: false,
        }
    }
}

pub fn jina_endpoint_kind_for_path(request_path: &str) -> Option<JinaEndpointKind> {
    match request_path_without_query(request_path) {
        "/v1/embeddings" | "/jina/v1/embeddings" => Some(JinaEndpointKind::Embedding),
        "/v1/rerank" | "/jina/v1/rerank" => Some(JinaEndpointKind::Rerank),
        _ => None,
    }
}

pub fn openai_compatible_rerank_policy_for_path(
    request_path: &str,
    stream_requested: bool,
) -> Result<Option<JinaRequestPolicy>, JinaRouteParseError> {
    if request_path_without_query(request_path) != "/v1/rerank" {
        return Ok(None);
    }

    if stream_requested {
        return Err(JinaRouteParseError::StreamNotSupported);
    }

    Ok(Some(JinaRequestPolicy::no_stream(JinaEndpointKind::Rerank)))
}

pub fn jina_request_policy_for_path(
    request_path: &str,
    stream_requested: bool,
) -> Result<JinaRequestPolicy, JinaRouteParseError> {
    let endpoint =
        jina_endpoint_kind_for_path(request_path).ok_or(JinaRouteParseError::UnknownPath)?;

    if stream_requested {
        return Err(JinaRouteParseError::StreamNotSupported);
    }

    Ok(JinaRequestPolicy::no_stream(endpoint))
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
    fn embedding_path_maps_to_no_stream_policy() {
        let policy = jina_request_policy_for_path("/v1/embeddings", false)
            .unwrap_or_else(|_| panic!("embedding path parses"));

        assert_eq!(policy.endpoint, JinaEndpointKind::Embedding);
        assert!(!policy.stream_allowed);
    }

    #[test]
    fn rerank_path_maps_to_no_stream_policy() {
        let policy = jina_request_policy_for_path("/v1/rerank?model=jina-reranker-v2-base", false)
            .unwrap_or_else(|_| panic!("rerank path parses"));

        assert_eq!(policy.endpoint, JinaEndpointKind::Rerank);
        assert!(!policy.stream_allowed);
    }

    #[test]
    fn native_jina_rerank_path_maps_to_no_stream_policy() {
        let policy = jina_request_policy_for_path("/jina/v1/rerank", false)
            .unwrap_or_else(|_| panic!("native Jina rerank path parses"));

        assert_eq!(policy.endpoint, JinaEndpointKind::Rerank);
        assert!(!policy.stream_allowed);
    }

    #[test]
    fn openai_compatible_rerank_path_maps_to_jina_policy() {
        let policy =
            match openai_compatible_rerank_policy_for_path("/v1/rerank?model=rerank-1", false) {
                Ok(Some(policy)) => policy,
                Ok(None) => panic!("OpenAI-compatible rerank path maps to Jina policy"),
                Err(_) => panic!("OpenAI-compatible rerank stream policy parses"),
            };

        assert_eq!(policy.endpoint, JinaEndpointKind::Rerank);
        assert!(!policy.stream_allowed);
    }

    #[test]
    fn embeddings_do_not_match_openai_compatible_rerank_helper() {
        assert_eq!(
            openai_compatible_rerank_policy_for_path("/v1/embeddings", false),
            Ok(None)
        );
        assert_eq!(
            openai_compatible_rerank_policy_for_path("/jina/v1/embeddings", false),
            Ok(None)
        );
    }

    #[test]
    fn unknown_path_is_rejected() {
        assert_eq!(
            jina_request_policy_for_path("/v1/chat/completions", false),
            Err(JinaRouteParseError::UnknownPath)
        );
        assert_eq!(jina_endpoint_kind_for_path("/v1/rerankings"), None);
    }

    #[test]
    fn stream_requests_are_rejected_for_known_jina_paths() {
        assert_eq!(
            jina_request_policy_for_path("/v1/embeddings", true),
            Err(JinaRouteParseError::StreamNotSupported)
        );
        assert_eq!(
            jina_request_policy_for_path("/v1/rerank", true),
            Err(JinaRouteParseError::StreamNotSupported)
        );
        assert_eq!(
            openai_compatible_rerank_policy_for_path("/v1/rerank", true),
            Err(JinaRouteParseError::StreamNotSupported)
        );
    }
}
