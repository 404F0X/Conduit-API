pub const AI_SDK_DATA_STREAM_CONTENT_TYPE: &str = "text/plain; charset=utf-8";
pub const AI_SDK_JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";

const AI_SDK_DATA_STREAM_MEDIA_TYPES: [&str; 3] =
    ["text/plain", "text/event-stream", "application/x-ndjson"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiSdkResponseContentType {
    DataStream,
    Json,
}

impl AiSdkResponseContentType {
    pub const fn as_header_value(self) -> &'static str {
        match self {
            Self::DataStream => AI_SDK_DATA_STREAM_CONTENT_TYPE,
            Self::Json => AI_SDK_JSON_CONTENT_TYPE,
        }
    }
}

pub fn is_aisdk_data_stream_content_type(content_type: &str) -> bool {
    let media_type = content_type
        .split_once(';')
        .map_or(content_type, |(media_type, _)| media_type)
        .trim();

    AI_SDK_DATA_STREAM_MEDIA_TYPES
        .iter()
        .any(|known| media_type.eq_ignore_ascii_case(known))
}

pub fn select_aisdk_response_content_type(accept: Option<&str>) -> AiSdkResponseContentType {
    let Some(accept) = accept else {
        return AiSdkResponseContentType::Json;
    };

    if accept.split(',').any(is_aisdk_data_stream_content_type) {
        AiSdkResponseContentType::DataStream
    } else {
        AiSdkResponseContentType::Json
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_data_stream_content_types() {
        assert!(is_aisdk_data_stream_content_type(
            "text/plain; charset=utf-8"
        ));
        assert!(is_aisdk_data_stream_content_type("text/event-stream"));
        assert!(is_aisdk_data_stream_content_type(
            "application/x-ndjson; q=0.9"
        ));
    }

    #[test]
    fn selector_uses_data_stream_for_supported_accept_header() {
        let selected =
            select_aisdk_response_content_type(Some("application/json, text/plain; q=0.8"));

        assert_eq!(selected, AiSdkResponseContentType::DataStream);
        assert_eq!(selected.as_header_value(), AI_SDK_DATA_STREAM_CONTENT_TYPE);
    }

    #[test]
    fn selector_uses_json_fallback_without_data_stream_accept() {
        let selected = select_aisdk_response_content_type(Some("application/json"));

        assert_eq!(selected, AiSdkResponseContentType::Json);
        assert_eq!(selected.as_header_value(), AI_SDK_JSON_CONTENT_TYPE);
    }

    #[test]
    fn selector_uses_json_fallback_for_unknown_accept_header() {
        assert_eq!(
            select_aisdk_response_content_type(Some("application/xml")),
            AiSdkResponseContentType::Json
        );
        assert_eq!(
            select_aisdk_response_content_type(None),
            AiSdkResponseContentType::Json
        );
    }
}
