//! Failure-class taxonomy and exit-code mapping.

/// Classifies a plan-cycle run outcome into one of three severity levels.
///
/// | Variant  | Meaning                                      |
/// |----------|----------------------------------------------|
/// | `Green`  | Run succeeded (exit code 0).                 |
/// | `Orange` | Recoverable failure; retry is warranted (exit code 2). |
/// | `Red`    | Hard failure; no retry (exit code 3 or unknown). |
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureClass {
    /// The plan cycle completed successfully.
    Green,
    /// The plan cycle failed in a recoverable way.
    Orange,
    /// The plan cycle failed in a non-recoverable way.
    Red,
}

impl FailureClass {
    /// Maps a process exit code to a [`FailureClass`].
    ///
    /// - `0`  → [`Green`](FailureClass::Green)
    /// - `2`  → [`Orange`](FailureClass::Orange)
    /// - `3`  → [`Red`](FailureClass::Red)
    /// - any other value → [`Red`](FailureClass::Red)
    pub fn from_exit_code(code: i32) -> FailureClass {
        match code {
            0 => FailureClass::Green,
            2 => FailureClass::Orange,
            3 => FailureClass::Red,
            _ => FailureClass::Red,
        }
    }
}

impl std::fmt::Display for FailureClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FailureClass::Green => write!(f, "green"),
            FailureClass::Orange => write!(f, "orange"),
            FailureClass::Red => write!(f, "red"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FailureClass;

    #[test]
    fn from_exit_code_zero_is_green() {
        assert_eq!(FailureClass::from_exit_code(0), FailureClass::Green);
    }

    #[test]
    fn from_exit_code_two_is_orange() {
        assert_eq!(FailureClass::from_exit_code(2), FailureClass::Orange);
    }

    #[test]
    fn from_exit_code_three_is_red() {
        assert_eq!(FailureClass::from_exit_code(3), FailureClass::Red);
    }

    #[test]
    fn from_exit_code_unknown_is_red() {
        assert_eq!(FailureClass::from_exit_code(99), FailureClass::Red);
    }

    #[test]
    fn display_green() {
        assert_eq!(FailureClass::Green.to_string(), "green");
    }

    #[test]
    fn display_orange() {
        assert_eq!(FailureClass::Orange.to_string(), "orange");
    }

    #[test]
    fn display_red() {
        assert_eq!(FailureClass::Red.to_string(), "red");
    }
}
