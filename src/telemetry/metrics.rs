use metrics::{Unit, describe_counter, describe_gauge, describe_histogram};
pub const HTTP_REQUESTS_TOTAL: &str = "http_requests_total";
pub const HTTP_REQUEST_DURATION: &str = "http_request_duration_seconds";
pub const HTTP_IN_FLIGHT: &str = "http_requests_in_flight";
pub const UPSTREAM_REQUESTS_TOTAL: &str = "upstream_requests_total";
pub const UPSTREAM_REQUEST_DURATION: &str = "upstream_request_duration_seconds";
pub const UPSTREAM_ERRORS_TOTAL: &str = "upstream_errors_total";

pub fn describe() {
    describe_counter!(HTTP_REQUESTS_TOTAL, "Total HTTP requests handled");
    describe_histogram!(
        HTTP_REQUEST_DURATION,
        Unit::Seconds,
        "End-to-end  
  request duration"
    );
    describe_gauge!(HTTP_IN_FLIGHT, "In-flight HTTP requests");
    describe_counter!(
        UPSTREAM_REQUESTS_TOTAL,
        "Upstream requests by       
  backend/status"
    );
    describe_histogram!(
        UPSTREAM_REQUEST_DURATION,
        Unit::Seconds,
        "Upstream
  hop duration"
    );
    describe_counter!(
        UPSTREAM_ERRORS_TOTAL,
        "Upstream failures by         
  backend/kind"
    );
}
