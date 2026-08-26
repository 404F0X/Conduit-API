//! Shared route-target health semantics.
//!
//! Both administrator reporting and runtime routing must use this classifier
//! so a target is never shown as healthy while the router considers it bad (or
//! vice versa). Persistence and aggregation live in the host/database layer.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteHealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

impl RouteHealthStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RouteHealthSample {
    pub attempts: i64,
    pub successes: i64,
    pub auth_failures: i64,
    pub configuration_failures: i64,
    pub transient_failures: i64,
}

pub fn classify_route_health(sample: RouteHealthSample) -> RouteHealthStatus {
    if sample.attempts <= 0 {
        return RouteHealthStatus::Unknown;
    }
    if sample.auth_failures > 0 || sample.configuration_failures > 0 {
        return RouteHealthStatus::Unhealthy;
    }

    let success_rate = sample.successes.max(0) as f64 / sample.attempts as f64;
    if sample.attempts >= 3 && success_rate < 0.5 {
        RouteHealthStatus::Unhealthy
    } else if sample.transient_failures > 0 || success_rate < 0.95 {
        RouteHealthStatus::Degraded
    } else {
        RouteHealthStatus::Healthy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_classification_prioritizes_permanent_failures() {
        assert_eq!(
            classify_route_health(RouteHealthSample::default()),
            RouteHealthStatus::Unknown
        );
        assert_eq!(
            classify_route_health(RouteHealthSample {
                attempts: 10,
                successes: 9,
                auth_failures: 1,
                ..Default::default()
            }),
            RouteHealthStatus::Unhealthy
        );
        assert_eq!(
            classify_route_health(RouteHealthSample {
                attempts: 10,
                successes: 9,
                transient_failures: 1,
                ..Default::default()
            }),
            RouteHealthStatus::Degraded
        );
        assert_eq!(
            classify_route_health(RouteHealthSample {
                attempts: 1,
                successes: 1,
                ..Default::default()
            }),
            RouteHealthStatus::Healthy
        );
    }
}
