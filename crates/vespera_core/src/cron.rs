//! Cron expression parsing shared by the compile-time `#[cron]` validator and
//! the runtime scheduler, so an expression accepted at compile time can never
//! be rejected (or scheduled differently) at runtime.

use croner::parser::{CronParser, Seconds};
pub use croner::{Cron, errors::CronError};

/// Parse a 6-field (`sec min hour day month weekday`, optional trailing year)
/// cron expression.
///
/// Legacy `1/10` / `/10` steps are accepted, and day-of-month and day-of-week
/// must both match when both are restricted.
pub fn parse(expression: &str) -> Result<Cron, CronError> {
    CronParser::builder()
        .seconds(Seconds::Required)
        .sloppy_ranges(true)
        .dom_and_dow(true)
        .build()
        .parse(expression)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("0 */5 * * * *")]
    #[case("1/10 * * * * *")]
    #[case("/10 * * * * *")]
    #[case("0 0 0 * * *")]
    #[case("0 30 9 * * Mon-Fri")]
    #[case("0 0 0 LW * *")]
    #[case("0 0 0 1 1 * 2099")]
    fn accepts_six_and_seven_field_expressions(#[case] expression: &str) {
        assert!(parse(expression).is_ok(), "{expression}");
    }

    #[rstest]
    #[case("")]
    #[case("invalid")]
    #[case("* * * * *")]
    #[case("0 99 * * * *")]
    #[case("not a cron at all")]
    fn rejects_malformed_expressions(#[case] expression: &str) {
        assert!(parse(expression).is_err(), "{expression}");
    }
}
